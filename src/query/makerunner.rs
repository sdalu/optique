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
    /// Whose `USER` and `HOME` the query sees.
    pub identity: Identity,
    /// The framework's `_EXPORTED_VARS` as [`framework_values`] found them,
    /// given on every query's command line.
    pub framework: Vec<(String, String)>,
}

/// Who runs the query, as far as make can tell from the environment:
/// `USER` and `HOME`. bsd.ccache.mk reads `USER`: under
/// `WITH_CCACHE_BUILD` with no `CCACHE_DIR`, it falls back to
/// /root/.ccache for root, and anyone else gets ccache disabled with a
/// WARNING and no devel/ccache build dependency.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Identity {
    /// The invoking user, whose own make runs a local build: `USER` and
    /// `HOME` as optique was given them, when set.
    Invoker,
    /// A builder's root: `USER=root` and `HOME=/root`, as poudriere's
    /// `jail_start` exports them (`injail` keeps them) and synth's
    /// `scan_environment` passes them.
    Root,
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

/// The only variables of optique's environment a query sees, when set.
/// PKG_DBDIR is pkg's, not make's, but `pkg which` runs at parse time
/// (Mk/bsd.default-versions.mk) and the cache key already folds it in.
/// `USER` and `HOME` follow the query's [`Identity`] ([`identity_env`]).
const PASSED_ENV: [&str; 3] = ["PATH", "TMPDIR", "PKG_DBDIR"];

/// `USER` and `HOME` for a query run as `identity`: root's for a builder,
/// optique's own (`invoker`, when set) for the invoking user.
fn identity_env(
    identity: Identity,
    invoker: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> Vec<(&'static str, std::ffi::OsString)> {
    match identity {
        Identity::Root => vec![("USER", "root".into()), ("HOME", "/root".into())],
        Identity::Invoker => ["USER", "HOME"]
            .into_iter()
            .filter_map(|var| invoker(var).map(|value| (var, value)))
            .collect(),
    }
}

/// `make -C dir` as every query runs it: the tree, the options dir and the
/// make.conf given, and nothing of optique's environment but what any make
/// needs; stdin, stdout and stderr piped.
fn make_command(ctx: &QueryCtx, dir: &Path) -> Command {
    // Given rather than left to make, which would take a PORTSDIR from the
    // environment or /etc/make.conf over this tree; resolved as make's own
    // autodetection from the port's directory would (`:tA`).
    let portsdir = ctx.portsdir.canonicalize().unwrap_or_else(|_| ctx.portsdir.clone());
    let mut cmd = bare_make(ctx.identity, ctx.make_conf.as_deref());
    cmd.arg("-C")
        .arg(dir)
        .arg(format!("PORTSDIR={}", portsdir.display()))
        .arg(format!("PORT_DBDIR={}", ctx.port_dbdir.display()))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

/// `/usr/bin/make` in the environment every query runs in, `make_conf` as
/// its `__MAKE_CONF` when given.
fn bare_make(identity: Identity, make_conf: Option<&Path>) -> Command {
    let mut cmd = Command::new("/usr/bin/make");
    // Nothing of the caller's environment but what any make needs to run:
    // an exported make variable (OPTIONS_SET, MAKEFLAGS, ...) would change
    // the answer without changing the cache key, and no builder sees it. What
    // a query needs is given here or staged in the make.conf: the overlays,
    // for one, reach make through the make.conf, never on the command line,
    // where OVERLAYS= would replace make.conf's `+=` list; a local build's
    // environment OVERLAYS has been written into its make.conf, where it
    // keeps its place after --overlay-dir and its absolute path. USER and
    // HOME are those of whoever runs the build: root in a builder, the
    // invoking user for a local build.
    cmd.env_clear();
    for var in PASSED_ENV {
        if let Some(value) = std::env::var_os(var) {
            cmd.env(var, value);
        }
    }
    cmd.envs(identity_env(identity, |var| std::env::var_os(var)));
    if let Some(conf) = make_conf {
        cmd.arg(format!("__MAKE_CONF={}", conf.display()));
    }
    cmd
}

/// What make.conf alone makes of `PORT_DBDIR` and `OPTIONS_FILE`: one make
/// with no port and neither on its command line, under the queries' own
/// environment (`identity`) and `make_conf`. Outside bsd.port.mk only
/// make.conf sets them (sys.mk sets neither). `PORT_DBDIR` comes expanded,
/// as a build uses it; `OPTIONS_FILE` as written, its `${OPTIONS_NAME}`
/// meaning nothing outside a port. Empty is unset; None when make cannot
/// be asked or fails.
pub fn options_place(identity: Identity, make_conf: Option<&Path>) -> Option<(String, String)> {
    let mut cmd = bare_make(identity, make_conf);
    // -dV prints a variable as written; a name holding `$` is expanded
    // all the same.
    cmd.current_dir("/")
        .args(["-f", "/dev/null", "-dV", "-V", "${PORT_DBDIR}", "-V", "OPTIONS_FILE"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let out = cmd.output().ok().filter(|o| o.status.success())?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut lines = text.lines().map(|l| l.trim().to_string());
    Some((lines.next()?, lines.next()?))
}

/// The framework's `_EXPORTED_VARS`, read once from the probe port of
/// [`wrapper::FRAMEWORK_PROBE`] under `ctx`'s tree, make.conf and
/// environment, for [`QueryCtx::framework`]: given on each query's command
/// line, they spare it bsd.port.mk's `uname`, `sysctl`, param.h, pkg(8)
/// and perl probes, as bsd.port.mk spares its own sub-makes. A value that
/// make would expand again is left out, and so is everything when make
/// fails: the queries then probe for themselves, as before.
pub fn framework_values(ctx: &QueryCtx) -> Vec<(String, String)> {
    let mut cmd = make_command(ctx, &ctx.portsdir);
    cmd.args(["-f", "/dev/stdin", "optique-probe"]);
    let Ok(mut child) = cmd.spawn() else { return Vec::new() };
    let fed = child
        .stdin
        .take()
        .expect("stdin piped")
        .write_all(wrapper::FRAMEWORK_PROBE.as_bytes());
    let Ok(out) = child.wait_with_output() else { return Vec::new() };
    if fed.is_err() || !out.status.success() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&out.stderr);
    text.lines()
        .filter_map(|line| {
            let rest = &line[line.find("OPTIQUE|")? + "OPTIQUE|".len()..];
            let (var, value) = rest.split_once('|')?;
            (!var.is_empty() && !value.contains('$')).then(|| (var.to_string(), value.to_string()))
        })
        .collect()
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

    let mut cmd = make_command(ctx, &portdir);
    cmd.args(["-f", "/dev/stdin", "optique-config"])
        .arg(format!("PORTDIR={}", portdir.display()))
        .args(ctx.framework.iter().map(|(var, value)| format!("{var}={value}")));
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
    let mut info = parse::parse_dump(key, &text)?;
    info.makefiles = tree_files(&info.makefiles, &portdir, &ctx.portsdir);
    Ok(info)
}

/// The files of `.MAKE.MAKEFILES` (`made`) that lie in the main tree,
/// relative to it, sorted: a relative name is the port directory's, `..`
/// is resolved by name, as make names a slave's master (`cat/slave/../
/// master/Makefile`), and the tree is matched as given and as resolved,
/// PORTSDIR reaching make canonicalised and PORTDIR not.
fn tree_files(made: &[String], portdir: &Path, portsdir: &Path) -> Vec<String> {
    let trees = [portsdir.canonicalize().ok(), Some(portsdir.to_path_buf())];
    let mut files: Vec<String> = made
        .iter()
        .filter_map(|name| {
            let mut path = PathBuf::new();
            for part in portdir.join(name).components() {
                match part {
                    std::path::Component::ParentDir => {
                        path.pop();
                    }
                    std::path::Component::CurDir => {}
                    other => path.push(other),
                }
            }
            let rel = trees.iter().flatten().find_map(|tree| path.strip_prefix(tree).ok())?;
            Some(rel.to_str()?.to_string())
        })
        .collect();
    files.sort();
    files.dedup();
    files
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
            identity: Identity::Invoker,
            framework: Vec::new(),
        };
        (tmp, ctx)
    }

    /// A query sees none of optique's environment but PATH, TMPDIR,
    /// PKG_DBDIR, and the USER and HOME of its identity: an exported make
    /// variable (OPTIONS_SET, say) is no input the cache keys, and no builder
    /// would see it. cargo exports CARGO_MANIFEST_DIR to every test, so it
    /// stands for any such variable here.
    #[test]
    fn a_query_sees_no_variable_of_the_environment_but_path_and_its_identity() {
        let _lock = make_test_lock();
        assert!(std::env::var_os("CARGO_MANIFEST_DIR").is_some(), "run under cargo test");
        assert!(std::env::var_os("PATH").is_some());
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("ports/cat/env");
        std::fs::create_dir_all(&dir).unwrap();
        // Each of USER and HOME as `want` has it ("" for unset): `same`
        // when make sees exactly that, `other` otherwise.
        let is = |var: &str, want: &str| {
            if want.is_empty() {
                format!("!defined({var})")
            } else {
                format!("defined({var}) && \"${{{var}}}\" == \"{want}\"")
            }
        };
        let makefile = |user: &str, home: &str| {
            format!(
                ".if {}\nU=same\n.else\nU=other\n.endif\n.if {}\nH=same\n.else\nH=other\n.endif\n\
                 PKGNAME=${{CARGO_MANIFEST_DIR:Dleak:Uclean}}-${{PATH:Dpath:Unopath}}-${{U}}-${{H}}-1.0\n",
                is("USER", user),
                is("HOME", home),
            )
        };
        let mut ctx = QueryCtx {
            portsdir: tmp.path().join("ports"),
            make_conf: None,
            port_dbdir: tmp.path().to_path_buf(),
            overlays: Vec::new(),
            identity: Identity::Root,
            framework: Vec::new(),
        };
        let key = PortKey::parse("cat/env").unwrap();
        std::fs::write(dir.join("Makefile"), makefile("root", "/root")).unwrap();
        assert_eq!(run_query(&ctx, &key).unwrap().pkgname, "clean-path-same-same-1.0");
        // The invoker's own, whatever the test runs as, set or not.
        ctx.identity = Identity::Invoker;
        let env = |var| std::env::var(var).unwrap_or_default();
        std::fs::write(dir.join("Makefile"), makefile(&env("USER"), &env("HOME"))).unwrap();
        assert_eq!(run_query(&ctx, &key).unwrap().pkgname, "clean-path-same-same-1.0");
    }

    /// USER and HOME by identity: root's whatever optique runs as, or the
    /// invoker's, each only when set.
    #[test]
    fn identity_env_gives_root_or_the_invokers_own() {
        let invoker = |var: &str| (var == "USER").then(|| "alice".into());
        let root = identity_env(Identity::Root, invoker);
        assert_eq!(root, [("USER", "root".into()), ("HOME", "/root".into())]);
        assert_eq!(identity_env(Identity::Invoker, invoker), [("USER", "alice".into())]);
        assert!(identity_env(Identity::Invoker, |_| None).is_empty());
    }

    /// Of what make read, the main tree's files are kept, relative to it:
    /// the port's own named relatively, a master's through `..`, the tree
    /// as given or resolved; the rest (the make.conf, sys.mk, the options
    /// file) is left out.
    #[test]
    fn tree_files_keeps_the_main_trees_files_relative_to_it() {
        let tmp = tempfile::tempdir().unwrap();
        let ports = tmp.path().join("ports");
        std::fs::create_dir_all(ports.join("cat/slave")).unwrap();
        let real = ports.canonicalize().unwrap();
        let made: Vec<String> = [
            "/dev/stdin".to_string(),
            "/usr/share/mk/sys.mk".to_string(),
            format!("{}/cat/slave/Makefile", ports.display()),
            format!("{}/cat/slave/../master/Makefile", ports.display()),
            format!("{}/Mk/bsd.port.mk", real.display()),
            "Makefile.local".to_string(),
            "/var/db/ports/cat_slave/options".to_string(),
        ]
        .into();
        let got = tree_files(&made, &ports.join("cat/slave"), &ports);
        let want =
            ["Mk/bsd.port.mk", "cat/master/Makefile", "cat/slave/Makefile", "cat/slave/Makefile.local"];
        assert_eq!(got, want);
    }

    /// A tree whose Mk/bsd.port.mk is `framework`, and a context on it.
    fn framework_tree(framework: &str) -> (tempfile::TempDir, QueryCtx) {
        let tmp = tempfile::tempdir().unwrap();
        let mk = tmp.path().join("ports/Mk");
        std::fs::create_dir_all(&mk).unwrap();
        std::fs::write(mk.join("bsd.port.mk"), framework).unwrap();
        let ctx = QueryCtx {
            portsdir: tmp.path().join("ports"),
            make_conf: None,
            port_dbdir: tmp.path().join("options"),
            overlays: Vec::new(),
            identity: Identity::Invoker,
            framework: Vec::new(),
        };
        (tmp, ctx)
    }

    /// The probe reports every non-empty `_EXPORTED_VARS` value, spaces
    /// kept, sorted, and leaves out an empty one, as bsd.port.mk does, and
    /// one make would expand again.
    #[test]
    fn framework_values_are_the_probes_exported_vars() {
        let _lock = make_test_lock();
        let (_tmp, ctx) = framework_tree(
            "ARCH=amd64\nPAIR=a b\nEMPTY=\nDOLLAR=$${HOME}\n\
             _EXPORTED_VARS+=PAIR ARCH EMPTY DOLLAR UNSET\n",
        );
        let got = framework_values(&ctx);
        let want = [("ARCH", "amd64"), ("PAIR", "a b")].map(|(v, x)| (v.to_string(), x.to_string()));
        assert_eq!(got, want);
    }

    /// An IGNOREd probe, or one make fails on, reports nothing: the
    /// queries then probe for themselves.
    #[test]
    fn framework_values_are_none_from_an_ignored_or_failed_probe() {
        let _lock = make_test_lock();
        let checked = "_PKG_CHECKED=1\n_EXPORTED_VARS+=_PKG_CHECKED\n";
        let (_tmp, ctx) = framework_tree(&format!("{checked}IGNORE=old pkg\n"));
        assert!(framework_values(&ctx).is_empty());
        let (_tmp, ctx) = framework_tree(&format!("{checked}.error broken\n"));
        assert!(framework_values(&ctx).is_empty());
        let (_tmp, ctx) = framework_tree("");
        std::fs::remove_file(ctx.portsdir.join("Mk/bsd.port.mk")).unwrap();
        assert!(framework_values(&ctx).is_empty());
    }

    /// A query gets the framework values on its command line, where they
    /// win over the port's own assignment as on a sub-make's `.MAKEFLAGS`.
    #[test]
    fn a_query_is_given_the_framework_values() {
        let _lock = make_test_lock();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("ports/cat/fw");
        std::fs::create_dir_all(&dir).unwrap();
        let makefile = "OSREL=0\nPKGNAME=${OSREL}-${PAIR:ts_}-1.0\n";
        std::fs::write(dir.join("Makefile"), makefile).unwrap();
        let ctx = QueryCtx {
            portsdir: tmp.path().join("ports"),
            make_conf: None,
            port_dbdir: tmp.path().to_path_buf(),
            overlays: Vec::new(),
            identity: Identity::Invoker,
            framework: vec![("OSREL".into(), "15.1".into()), ("PAIR".into(), "a b".into())],
        };
        let key = PortKey::parse("cat/fw").unwrap();
        assert_eq!(run_query(&ctx, &key).unwrap().pkgname, "15.1-a_b-1.0");
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
        // A sleep this test process alone runs: pgrep sees the whole host,
        // where another run of the suite has a grandchild of its own.
        let nap = format!("37.{:06}", std::process::id() % 1_000_000);
        let (_tmp, ctx) = slow_port_running(&format!("sh -c 'sleep {nap}; true'"));
        let runner = MakeRunner::new(ctx, 1);
        runner.submit(PortKey::parse("cat/slow").unwrap());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let pattern = format!("^sleep {}$", nap.replace('.', "\\."));
        let sleeping = || {
            std::process::Command::new("pgrep")
                .args(["-f", &pattern])
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
            identity: Identity::Invoker,
            framework: Vec::new(),
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

    /// Under `WITH_CCACHE_BUILD` with no `CCACHE_DIR`, bsd.ccache.mk keeps
    /// ccache for root alone: a builder's query, run as root, has
    /// devel/ccache as a build dependency; a query run as anyone else has
    /// ccache disabled, and no such dependency.
    /// Run with: cargo test -- --ignored
    #[test]
    #[ignore = "needs /usr/ports on a FreeBSD host"]
    fn live_ccache_follows_the_identity() {
        let _lock = make_test_lock();
        let staging = tempfile::tempdir().unwrap();
        let make_conf = staging.path().join("make.conf");
        std::fs::write(&make_conf, "WITH_CCACHE_BUILD=yes\n").unwrap();
        let mut ctx = QueryCtx {
            portsdir: "/usr/ports".into(),
            make_conf: Some(make_conf),
            port_dbdir: staging.path().to_path_buf(),
            overlays: Vec::new(),
            identity: Identity::Root,
            framework: Vec::new(),
        };
        let key = PortKey::parse("sysutils/tmux").unwrap();
        let ccache = |info: &PortInfo| info.deps.iter().any(|d| d.target.origin == "devel/ccache");
        let root = run_query(&ctx, &key).unwrap();
        assert!(ccache(&root), "root keeps ccache: {:?}", root.deps);
        if std::env::var("USER").is_ok_and(|u| u != "root") {
            ctx.identity = Identity::Invoker;
            let user = run_query(&ctx, &key).unwrap();
            assert!(!ccache(&user), "ccache disabled for a user: {:?}", user.deps);
        }
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
            identity: Identity::Root,
            framework: Vec::new(),
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
            identity: Identity::Root,
            framework: Vec::new(),
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
