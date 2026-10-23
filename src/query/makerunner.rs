use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::JoinHandle;

use crossbeam_channel::{Receiver, Sender};

use crate::model::origin::PortKey;
use crate::model::port::PortInfo;
use crate::query::{parse, wrapper};

/// Everything a worker needs to run one make query.
#[derive(Clone, Debug)]
pub struct QueryCtx {
    pub portsdir: PathBuf,
    /// Layered make.conf passed as __MAKE_CONF (None = leave make's default).
    pub make_conf: Option<PathBuf>,
    /// PORT_DBDIR the query should see (real options dir, or staging).
    pub port_dbdir: PathBuf,
    /// Trees laid over `portsdir`, in lookup order (`config::Settings`).
    pub overlays: Vec<PathBuf>,
}

impl QueryCtx {
    /// Where a port lives: the first overlay holding `<origin>/Makefile`,
    /// else the main tree — the ports framework's own lookup (do-depends.sh,
    /// depends-list.sh) and poudriere's (`_lookup_portdir`). None when no
    /// tree has it.
    pub fn locate(&self, origin: &str) -> Option<PathBuf> {
        locate(&self.portsdir, &self.overlays, origin)
    }
}

/// `QueryCtx::locate` for callers holding the trees but no context.
pub fn locate(portsdir: &Path, overlays: &[PathBuf], origin: &str) -> Option<PathBuf> {
    overlays
        .iter()
        .map(PathBuf::as_path)
        .chain(std::iter::once(portsdir))
        .map(|tree| tree.join(origin))
        .find(|dir| dir.join("Makefile").is_file())
}

#[derive(Debug)]
pub enum ScanEvent {
    PortDone(Box<PortInfo>),
    PortError { key: PortKey, msg: String },
}

/// Fixed pool of worker threads turning PortKeys into ScanEvents.
pub struct MakeRunner {
    job_tx: Sender<PortKey>,
    pub events: Receiver<ScanEvent>,
    workers: Vec<JoinHandle<()>>,
}

impl MakeRunner {
    pub fn new(ctx: QueryCtx, jobs: usize) -> Self {
        let (job_tx, job_rx) = crossbeam_channel::unbounded::<PortKey>();
        let (ev_tx, ev_rx) = crossbeam_channel::unbounded::<ScanEvent>();
        let workers = (0..jobs.max(1))
            .map(|_| {
                let job_rx = job_rx.clone();
                let ev_tx = ev_tx.clone();
                let ctx = ctx.clone();
                std::thread::spawn(move || {
                    for key in job_rx.iter() {
                        let ev = match run_query(&ctx, &key) {
                            Ok(info) => ScanEvent::PortDone(Box::new(info)),
                            Err(e) => ScanEvent::PortError { key: key.clone(), msg: format!("{e:#}") },
                        };
                        if ev_tx.send(ev).is_err() {
                            break;
                        }
                    }
                })
            })
            .collect();
        MakeRunner { job_tx, events: ev_rx, workers }
    }

    pub fn submit(&self, key: PortKey) {
        let _ = self.job_tx.send(key);
    }

    /// Close the job queue and wait for workers to drain.
    pub fn shutdown(self) {
        drop(self.job_tx);
        for w in self.workers {
            let _ = w.join();
        }
    }
}

/// Run the wrapper makefile for one port@flavor and parse the dump.
pub fn run_query(ctx: &QueryCtx, key: &PortKey) -> anyhow::Result<PortInfo> {
    let Some(portdir) = ctx.locate(&key.origin) else {
        let missing = ctx.portsdir.join(&key.origin).join("Makefile");
        match ctx.overlays.len() {
            0 => anyhow::bail!("no such port: {} ({} missing)", key.origin, missing.display()),
            n => anyhow::bail!(
                "no such port: {} (not in {}, nor in {n} overlay(s))",
                key.origin,
                ctx.portsdir.display()
            ),
        }
    };

    // Given rather than left to make, which would take a PORTSDIR from the
    // environment or /etc/make.conf over this tree; resolved as make's own
    // autodetection from the port's directory would (`:tA`).
    let portsdir = ctx.portsdir.canonicalize().unwrap_or_else(|_| ctx.portsdir.clone());
    let mut cmd = Command::new("/usr/bin/make");
    cmd.arg("-C")
        .arg(&portdir)
        .args(["-f", "/dev/stdin", "optique-config"])
        .arg(format!("PORTSDIR={}", portsdir.display()))
        .arg(format!("PORTDIR={}", portdir.display()))
        .arg(format!("PORT_DBDIR={}", ctx.port_dbdir.display()))
        // Don't let the caller's environment distort the query.
        .env_remove("MAKEFLAGS")
        .env_remove("MAKELEVEL")
        .env_remove("MAKEOBJDIR")
        .env_remove("MAKEOBJDIRPREFIX")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // The overlays reach make through the make.conf, never on the command
    // line, where OVERLAYS= would replace make.conf's `+=` list, nor through
    // the environment: a local build's has been written into its make.conf,
    // where it keeps its place after --overlay-dir and its absolute path.
    cmd.env_remove("OVERLAYS");
    if let Some(conf) = &ctx.make_conf {
        cmd.arg(format!("__MAKE_CONF={}", conf.display()));
    }
    if let Some(flavor) = &key.flavor {
        cmd.arg(format!("FLAVOR={flavor}"));
    }

    let mut child = cmd.spawn()?;
    // Registered so a TUI quit can terminate whatever is still running
    // instead of leaving orphaned makes churning; the guard unregisters on
    // every exit path, including early ?-returns.
    let _active = ActiveMake::register(child.id());
    // The wrapper is far smaller than the pipe buffer, so a plain write
    // before reading the output cannot deadlock.
    child
        .stdin
        .take()
        .expect("stdin piped")
        .write_all(wrapper::WRAPPER.as_bytes())?;
    let out = child.wait_with_output()?;

    let mut text = String::from_utf8_lossy(&out.stderr).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stdout));

    // Checked before the dump: make reports the options file it cannot
    // open, then goes on and prints a full dump as if there were no file,
    // which would read as an unconfigured port whose file apply may replace.
    if let Some(why) = unreadable_options_file(&text, &ctx.port_dbdir) {
        anyhow::bail!("{why}");
    }
    // Any failure fails the port, dump or not: make carries on past a
    // .include it cannot find and prints a dump that is missing whatever the
    // file would have set. That file is named first, being the cause; the
    // tail would only show the dump.
    if !out.status.success() {
        let cause = text.lines().find(|l| l.contains("Could not find") || l.contains("Cannot open"));
        let why =
            cause.map_or_else(|| parse::last_lines(&text, 5, " | "), |l| l.trim().to_string());
        anyhow::bail!("make failed ({}): {why}", out.status);
    }
    parse::parse_dump(key, &text)
}

/// An options file make could not open: bsd.options.mk includes it once it
/// exists, and make says only "Cannot open <path>" before carrying on as if
/// it were missing (exit status 1, dump complete). When that path is in the
/// options dir, name the file and the real reason: it is the file that needs
/// fixing, and the port must not be taken for an unconfigured one.
fn unreadable_options_file(make_output: &str, port_dbdir: &Path) -> Option<String> {
    let path = make_output.lines().find_map(|line| {
        let path = line.split_once("Cannot open ")?.1.trim();
        Path::new(path).starts_with(port_dbdir).then_some(path)
    })?;
    let why = match std::fs::File::open(path) {
        Err(e) => e.to_string(),
        Ok(_) => "it could not be opened when make read it".to_string(),
    };
    Some(format!("cannot read options file {path}: {why}"))
}

/// PIDs of the make processes currently running, across every runner in the
/// process (foreground scans and background refreshes alike).
static ACTIVE_MAKES: std::sync::LazyLock<std::sync::Mutex<std::collections::HashSet<u32>>> =
    std::sync::LazyLock::new(Default::default);

/// Registration guard: the pid leaves the registry when the query is done,
/// whichever way it ends.
struct ActiveMake(u32);

impl ActiveMake {
    fn register(pid: u32) -> Self {
        ACTIVE_MAKES.lock().unwrap().insert(pid);
        ActiveMake(pid)
    }
}

impl Drop for ActiveMake {
    fn drop(&mut self) {
        ACTIVE_MAKES.lock().unwrap().remove(&self.0);
    }
}

/// How many make processes are running right now.
pub fn active_make_count() -> usize {
    ACTIVE_MAKES.lock().unwrap().len()
}

/// Terminate every registered make process (the quit path). Returns how many
/// were signalled; their workers see the death as a failed query and drain.
///
/// Everything they run goes with them: a shell command make runs while
/// parsing (`!=`, `:sh`) inherits the query's output pipe, so it, or anything
/// it started in turn, would hold the query open and outlive optique after
/// make itself was gone. The whole tree is listed while it is still a tree
/// (an orphan is re-parented and lost to the walk), then signalled in one
/// `kill`: a child killed first would only let make go on and finish.
pub fn kill_active_makes() -> usize {
    let pids: Vec<u32> = ACTIVE_MAKES.lock().unwrap().iter().copied().collect();
    if pids.is_empty() {
        return 0;
    }
    let targets: Vec<u32> = descendants(&pids, &process_table()).into_iter().chain(pids.iter().copied()).collect();
    for pid in targets {
        if let Ok(pid) = libc::pid_t::try_from(pid) {
            // SAFETY: kill(2) takes plain integers; a pid gone meanwhile is
            // only an ESRCH, ignored like the rest.
            unsafe { libc::kill(pid, libc::SIGTERM) };
        }
    }
    pids.len()
}

/// Every process as a (pid, parent pid) pair, from the kernel's process
/// table (the `kern.proc.proc` sysctl ps(1) reads too). Each record is
/// stepped over by the size the kernel gives it, not by libc's `kinfo_proc`,
/// which is one FreeBSD version's layout; only the pid and parent pid are
/// read, which sit at the same place in all of them. Empty on failure.
#[cfg(target_os = "freebsd")]
fn process_table() -> Vec<(u32, u32)> {
    use std::mem::{offset_of, size_of};
    let mib = [libc::CTL_KERN, libc::KERN_PROC, libc::KERN_PROC_PROC];
    // The table can grow between asking its size and reading it: room for
    // growth, and another try when that was not enough.
    for _ in 0..4 {
        let mut len: libc::size_t = 0;
        // SAFETY: a size query, no buffer.
        let sized = unsafe {
            libc::sysctl(mib.as_ptr(), 3, std::ptr::null_mut(), &mut len, std::ptr::null(), 0)
        };
        if sized != 0 {
            return Vec::new();
        }
        len += len / 4;
        let mut buf = vec![0u8; len];
        // SAFETY: `buf` holds `len` bytes, and sysctl writes at most that.
        let read = unsafe {
            libc::sysctl(mib.as_ptr(), 3, buf.as_mut_ptr().cast(), &mut len, std::ptr::null(), 0)
        };
        if read != 0 {
            if std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOMEM) {
                continue;
            }
            return Vec::new();
        }
        buf.truncate(len);
        let field = |record: &[u8], at: usize| -> Option<i32> {
            let bytes = record.get(at..at + size_of::<i32>())?;
            Some(i32::from_ne_bytes(bytes.try_into().ok()?))
        };
        let (size_at, pid_at, ppid_at) = (
            offset_of!(libc::kinfo_proc, ki_structsize),
            offset_of!(libc::kinfo_proc, ki_pid),
            offset_of!(libc::kinfo_proc, ki_ppid),
        );
        let mut out = Vec::new();
        let mut rest = &buf[..];
        while let Some(size) = field(rest, size_at) {
            let Ok(size) = usize::try_from(size) else { break };
            if size == 0 || size > rest.len() {
                break;
            }
            let record = &rest[..size];
            if let (Some(pid), Some(ppid)) = (field(record, pid_at), field(record, ppid_at)) {
                if let (Ok(pid), Ok(ppid)) = (u32::try_from(pid), u32::try_from(ppid)) {
                    out.push((pid, ppid));
                }
            }
            rest = &rest[size..];
        }
        return out;
    }
    Vec::new()
}

/// Elsewhere only make itself is signalled.
#[cfg(not(target_os = "freebsd"))]
fn process_table() -> Vec<(u32, u32)> {
    Vec::new()
}

/// Every descendant of `roots` in a (pid, parent pid) table, nearest first.
fn descendants(roots: &[u32], table: &[(u32, u32)]) -> Vec<u32> {
    let mut children: std::collections::HashMap<u32, Vec<u32>> = Default::default();
    for &(pid, ppid) in table {
        children.entry(ppid).or_default().push(pid);
    }
    // Seeded with the roots so neither a root nor a pid already queued is
    // added again; `insert` reporting "new" is the membership check.
    let mut seen: std::collections::HashSet<u32> = roots.iter().copied().collect();
    let mut out = Vec::new();
    let mut queue: std::collections::VecDeque<u32> = roots.iter().copied().collect();
    while let Some(pid) = queue.pop_front() {
        for &child in children.get(&pid).into_iter().flatten() {
            if seen.insert(child) {
                out.push(child);
                queue.push_back(child);
            }
        }
    }
    out
}

/// Tests that run make hold this: the registry of running makes is
/// process-wide, and one test kills whatever it finds there.
#[cfg(test)]
pub(crate) fn make_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::origin::PortKey;

    /// A port whose Makefile takes `secs` to parse, in a tree of its own.
    fn slow_port(secs: u32) -> (tempfile::TempDir, QueryCtx) {
        slow_port_running(&format!("sleep {secs}"))
    }

    /// A port whose Makefile runs `command` while it is parsed.
    fn slow_port_running(command: &str) -> (tempfile::TempDir, QueryCtx) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("ports/cat/slow");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Makefile"), format!("PKGNAME=slow-1.0\nX!= {command}\n")).unwrap();
        let ctx = QueryCtx {
            portsdir: tmp.path().join("ports"),
            make_conf: None,
            port_dbdir: tmp.path().to_path_buf(),
            overlays: Vec::new(),
        };
        (tmp, ctx)
    }

    #[test]
    fn descendants_walks_the_whole_tree_only() {
        // 10 is make; 11 and 12 its children; 13 a grandchild; 20 unrelated.
        let table = [(10, 1), (11, 10), (12, 10), (13, 11), (20, 1), (21, 20)];
        assert_eq!(descendants(&[10], &table), [11, 12, 13]);
        assert!(descendants(&[99], &table).is_empty());
        assert_eq!(descendants(&[11, 20], &table), [13, 21]);
    }

    /// The table read from the kernel knows this very process and its
    /// parent: the record walk and the field offsets are right.
    #[cfg(target_os = "freebsd")]
    #[test]
    fn process_table_finds_this_process_and_its_parent() {
        let table = process_table();
        let me = std::process::id();
        // SAFETY: getppid(2) cannot fail.
        let parent = u32::try_from(unsafe { libc::getppid() }).unwrap();
        assert!(table.contains(&(me, parent)), "{me}/{parent} not in {} records", table.len());
        assert!(table.len() > 10, "a real table: {} records", table.len());
        let child = std::process::Command::new("sleep").arg("5").spawn().unwrap();
        let mine = descendants(&[me], &process_table());
        let mut child = child;
        let _ = child.kill();
        let _ = child.wait();
        assert!(mine.contains(&child.id()), "the child is found as a descendant");
    }

    /// What make runs can run more: a shell with two commands forks the first
    /// rather than becoming it, so `sleep` is make's grandchild here. The
    /// whole tree goes, or the grandchild would hold the query open and
    /// outlive optique.
    #[test]
    fn kill_active_makes_reaches_grandchildren() {
        let _lock = make_test_lock();
        let (_tmp, ctx) = slow_port_running("sh -c 'sleep 37; true'");
        let runner = MakeRunner::new(ctx, 1);
        runner.submit(PortKey::parse("cat/slow").unwrap());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let sleeping = || {
            std::process::Command::new("pgrep")
                .args(["-f", "^sleep 37$"])
                .output()
                .map(|o| !o.stdout.is_empty())
                .unwrap_or(false)
        };
        while !sleeping() {
            assert!(std::time::Instant::now() < deadline, "the grandchild never started");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(kill_active_makes(), 1);
        let ev = runner.events.recv_timeout(std::time::Duration::from_secs(10)).expect("an event");
        assert!(matches!(ev, ScanEvent::PortError { .. }));
        runner.shutdown();
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(!sleeping(), "the grandchild outlived the kill");
    }

    /// Quitting kills the makes still running: the query ends at once, as a
    /// failure, and the registry empties. A query that ends by itself leaves
    /// the registry as it found it.
    #[test]
    fn kill_active_makes_stops_running_queries() {
        let _lock = make_test_lock();
        let (_tmp, ctx) = slow_port(0);
        run_query(&ctx, &PortKey::parse("cat/slow").unwrap()).unwrap();
        assert_eq!(active_make_count(), 0, "a finished query unregisters");
        assert_eq!(kill_active_makes(), 0);

        let (_tmp, ctx) = slow_port(30);
        let runner = MakeRunner::new(ctx, 1);
        runner.submit(PortKey::parse("cat/slow").unwrap());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while active_make_count() == 0 {
            assert!(std::time::Instant::now() < deadline, "make never started");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(active_make_count(), 1);
        let started = std::time::Instant::now();
        assert_eq!(kill_active_makes(), 1);
        let ev = runner.events.recv_timeout(std::time::Duration::from_secs(10)).expect("an event");
        assert!(matches!(ev, ScanEvent::PortError { .. }), "a killed query fails");
        assert!(started.elapsed() < std::time::Duration::from_secs(10), "not the 30 s sleep");
        runner.shutdown();
        assert_eq!(active_make_count(), 0);
    }

    /// Live test against /usr/ports: staging an options file that enables
    /// HTTP_PERL for www/nginx must change the queried dependency set.
    /// Run with: cargo test -- --ignored
    #[test]
    #[ignore = "needs /usr/ports on a FreeBSD host"]
    fn live_staged_option_changes_deps() {
        let _lock = make_test_lock();
        let staging = tempfile::tempdir().unwrap();
        let ctx = QueryCtx {
            portsdir: "/usr/ports".into(),
            make_conf: None,
            port_dbdir: staging.path().to_path_buf(),
            overlays: Vec::new(),
        };
        let key = PortKey::parse("www/nginx").unwrap();

        let before = run_query(&ctx, &key).expect("baseline query");
        assert!(!before.options.complete.is_empty());
        let had_perl =
            before.deps.iter().any(|d| d.target.origin.starts_with("lang/perl5"));
        assert!(!had_perl, "HTTP_PERL off by default");

        // Stage HTTP_PERL=on the way the TUI does.
        let mut enabled = before.options.effective.clone();
        enabled.insert("HTTP_PERL".to_string());
        let content = crate::optionsfile::render(
            &before.pkgname,
            &before.options.complete,
            &enabled,
        );
        let db = crate::staging::StagingDb::create(
            staging.path(),
            &crate::optfiles::OptionsFiles::new(staging.path()), // nothing to seed
            std::iter::empty(),
        )
        .unwrap();
        db.write(&before.options_name, content.as_bytes()).unwrap();

        let ctx2 = QueryCtx { port_dbdir: db.path().to_path_buf(), ..ctx };
        let after = run_query(&ctx2, &key).expect("staged query");
        assert!(after.options.effective.contains("HTTP_PERL"));
        assert!(
            after.deps.iter().any(|d| d.target.origin.starts_with("lang/perl5")),
            "enabling HTTP_PERL must add the perl dependency"
        );
    }

    /// In a poudriere jail's environment, what the host has installed must not
    /// reach the answer. Each half fails on a host carrying the package it
    /// names (apr without LDAP, libiconv), and passes on any host once the
    /// environment hides LOCALBASE.
    /// Run with: cargo test -- --ignored
    #[test]
    #[ignore = "needs /usr/ports on a FreeBSD host"]
    fn live_jail_env_hides_host_packages() {
        let _lock = make_test_lock();
        let staging = tempfile::tempdir().unwrap();
        let make_conf = crate::config::stage_jail_make_conf(staging.path()).unwrap();
        let db = crate::staging::StagingDb::create(
            staging.path(),
            &crate::optfiles::OptionsFiles::new(staging.path()), // nothing to seed
            std::iter::empty(),
        )
        .unwrap();
        let ctx = QueryCtx {
            portsdir: "/usr/ports".into(),
            make_conf: Some(make_conf),
            port_dbdir: db.path().to_path_buf(),
            overlays: Vec::new(),
        };

        // Mk/Uses/iconv.mk adds libiconv when ${LOCALBASE}/include/iconv.h
        // exists; a jail has none.
        let lame = run_query(&ctx, &PortKey::parse("audio/lame").unwrap()).unwrap();
        assert!(
            !lame.deps.iter().any(|d| d.target.origin == "converters/libiconv"),
            "the host's libiconv leaked into audio/lame: {:?}",
            lame.deps
        );

        // www/apache24 IGNOREs LDAP when the installed apr-util lacks it,
        // although poudriere would build apr with LDAP first.
        let key = PortKey::parse("www/apache24").unwrap();
        let before = run_query(&ctx, &key).unwrap();
        let mut enabled = before.options.effective.clone();
        enabled.insert("LDAP".to_string());
        let content = crate::optionsfile::render(
            &before.pkgname,
            &before.options.complete,
            &enabled,
        );
        db.write(&before.options_name, content.as_bytes()).unwrap();
        let after = run_query(&ctx, &key).unwrap();
        assert!(after.options.effective.contains("LDAP"));
        assert_eq!(after.ignore, None, "the host's apr-util leaked into www/apache24");
    }

    /// synth reads ports in a chroot with an empty /usr/local, so an
    /// installed PHP must not pick the PHP a port builds against: php.mk
    /// includes ${PHPBASE}/etc/php.conf when it exists, PHPBASE defaulting
    /// to LOCALBASE, and adds -zts to the extension directory when
    /// ${LOCALBASE}/sbin/httpd -V says threaded. A fake LOCALBASE with a
    /// php82 php.conf and a threaded httpd, set by the profile's make.conf,
    /// stands for the host's packages on any host.
    /// Run with: cargo test -- --ignored
    #[test]
    #[ignore = "needs /usr/ports on a FreeBSD host"]
    fn live_synth_env_hides_host_packages() {
        let _lock = make_test_lock();
        let staging = tempfile::tempdir().unwrap();
        let localbase = staging.path().join("localbase");
        std::fs::create_dir_all(localbase.join("etc")).unwrap();
        std::fs::write(localbase.join("etc/php.conf"), "PHP_VER=82\nPHP_EXT_DIR=20220829\n")
            .unwrap();
        let httpd = localbase.join("sbin/httpd");
        std::fs::create_dir_all(httpd.parent().unwrap()).unwrap();
        std::fs::write(&httpd, "#!/bin/sh\necho 'Server MPM:     event'\necho '  threaded:     yes'\n")
            .unwrap();
        std::fs::set_permissions(&httpd, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        let profile = staging.path().join("Live-make.conf");
        std::fs::write(&profile, format!("LOCALBASE={}\n", localbase.display())).unwrap();
        let make_conf =
            crate::config::stage_synth_make_conf(staging.path(), "Live", &profile).unwrap();
        let db = crate::staging::StagingDb::create(
            staging.path(),
            &crate::optfiles::OptionsFiles::new(staging.path()), // nothing to seed
            std::iter::empty(),
        )
        .unwrap();
        let ctx = QueryCtx {
            portsdir: "/usr/ports".into(),
            make_conf: Some(make_conf),
            port_dbdir: db.path().to_path_buf(),
            overlays: Vec::new(),
        };

        let xml = run_query(&ctx, &PortKey::parse("textproc/php84-xml").unwrap()).unwrap();
        assert!(
            xml.deps.iter().any(|d| d.target.origin == "lang/php84"),
            "textproc/php84-xml must build against lang/php84: {:?}",
            xml.deps
        );
        assert!(
            !xml.deps.iter().any(|d| d.target.origin == "lang/php82"),
            "the host's php.conf leaked into textproc/php84-xml: {:?}",
            xml.deps
        );

        // A PHP application's extensions live in the plain extension
        // directory, not the -zts one a threaded httpd would pick.
        let pma = run_query(&ctx, &PortKey::parse("databases/phpmyadmin5").unwrap()).unwrap();
        assert!(pma.deps.iter().any(|d| d.spec.contains("/lib/php/")), "{:?}", pma.deps);
        assert!(
            !pma.deps.iter().any(|d| d.spec.contains("-zts/")),
            "the host's threaded httpd leaked into databases/phpmyadmin5: {:?}",
            pma.deps
        );
    }
}
