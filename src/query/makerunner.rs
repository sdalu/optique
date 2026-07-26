use std::io::Write as _;
use std::path::PathBuf;
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
    let portdir = ctx.portsdir.join(&key.origin);
    if !portdir.join("Makefile").is_file() {
        anyhow::bail!("no such port: {} ({} missing)", key.origin, portdir.display());
    }

    let mut cmd = Command::new("/usr/bin/make");
    cmd.arg("-C")
        .arg(&portdir)
        .args(["-f", "/dev/stdin", "optique-config"])
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
    if let Some(conf) = &ctx.make_conf {
        cmd.arg(format!("__MAKE_CONF={}", conf.display()));
    }
    if let Some(flavor) = &key.flavor {
        cmd.arg(format!("FLAVOR={flavor}"));
    }

    let mut child = cmd.spawn()?;
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

    if !out.status.success() && !text.contains("OPTIQUE|") {
        anyhow::bail!("make failed ({}): {}", out.status, last_lines(&text, 5));
    }
    parse::parse_dump(key, &text)
}

fn last_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join(" | ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::origin::PortKey;

    /// Live test against /usr/ports: staging an options file that enables
    /// HTTP_PERL for www/nginx must change the queried dependency set.
    /// Run with: cargo test -- --ignored
    #[test]
    #[ignore = "needs /usr/ports on a FreeBSD host"]
    fn live_staged_option_changes_deps() {
        let staging = tempfile::tempdir().unwrap();
        let ctx = QueryCtx {
            portsdir: "/usr/ports".into(),
            make_conf: None,
            port_dbdir: staging.path().to_path_buf(),
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
            staging.path(), // nothing to seed
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
}
