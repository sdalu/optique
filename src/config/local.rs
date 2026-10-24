//! A local build: `make config` on this host, its make.conf and its
//! PORTSDIR and PORT_DBDIR as make sees them.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use super::{
    checked_portsdir, is_ports_tree, make_values, stage_make_conf, Blacklist, HostPaths, Overlay, QueryEnv,
    Request, Settings,
};

/// PORTSDIR and PORT_DBDIR as make(1) sees them on this host, from the
/// environment or /etc/make.conf (sys.mk reads it), None where neither
/// sets one. make is asked rather than the environment read, so that a
/// make.conf setting counts as it does for `make config`; any failure
/// leaves the built-in defaults.
pub(super) fn make_defaults() -> (Option<PathBuf>, Option<PathBuf>) {
    let Some(text) = make_values(&["PORTSDIR", "PORT_DBDIR"], None, &[]) else {
        return (None, None);
    };
    let mut lines = text.lines().map(|l| {
        let l = l.trim();
        (!l.is_empty()).then(|| PathBuf::from(l))
    });
    (lines.next().flatten(), lines.next().flatten())
}

/// A local build: `make config` on this host, built here. What is
/// installed is then the truth, not a leak. The make.conf goes through
/// the staging like any layer, so that the cache keys its content and the
/// banner names it, but make reads the original: a copy elsewhere would
/// lose the directory its relative `.include`s are found from.
pub(super) fn resolve(
    host: &HostPaths,
    req: &Request,
    overlays: &[Overlay],
    staging_dir: &Path,
) -> Result<Settings> {
    if req.jail.is_some() || req.set.is_some() || req.tree.is_some() {
        bail!(
            "-j/-z/-p name poudriere's configuration, but {} does not exist",
            host.poudriere_d.display()
        );
    }
    let mut notes = Vec::new();
    let portsdir = match req.portsdir {
        Some(dir) => checked_portsdir(dir)?,
        None if is_ports_tree(host.ports) => {
            if host.ports_from_make {
                notes.push(format!("ports tree {} from PORTSDIR", host.ports.display()));
            }
            host.ports.to_path_buf()
        }
        None => bail!("no ports tree at {}; pass -P", host.ports.display()),
    };
    let options_dir = match req.options_dir {
        Some(d) => d.to_path_buf(),
        None => {
            if host.db_from_make {
                notes.push(format!("options dir {} from PORT_DBDIR", host.db.display()));
            }
            host.db.to_path_buf()
        }
    };
    let (staged, mut conf_hash, make_conf_sources) =
        stage_make_conf(String::new(), &[host.make_conf.to_path_buf()], "", staging_dir)?;
    // Queries run with a cleared environment, so make would fall back to
    // /etc/make.conf, unhashed, when the one it was given cannot be read:
    // /dev/null stands for "none", as an unreadable __MAKE_CONF is for make.
    let mut make_conf =
        Some(staged.map_or_else(|| PathBuf::from("/dev/null"), |_| host.make_conf.to_path_buf()));
    // --overlay-dir, then the environment's OVERLAYS (absolute), then the
    // make.conf's own: a small make.conf including the real one first,
    // still read in place for its own .includes, then putting them in
    // front of whatever it left, so a make.conf that assigns OVERLAYS=
    // rather than appending cannot drop them. The environment's go through
    // it, not through the queries' own environment, where bmake's += would
    // have put them first.
    let paths: Vec<String> = overlays
        .iter()
        .map(|o| o.path.display().to_string())
        .chain(host.env_overlays.iter().map(|p| p.display().to_string()))
        .collect();
    if !paths.is_empty() {
        let text = format!(
            ".sinclude \"{}\"\nOVERLAYS:={} ${{OVERLAYS}}\n",
            std::path::absolute(host.make_conf)?.display(),
            paths.join(" ")
        );
        let path = staging_dir.join("local-make.conf");
        fs::write(&path, &text).with_context(|| format!("writing {}", path.display()))?;
        conf_hash = crate::cache::sha256_hex(format!("{conf_hash}\n{text}").as_bytes());
        make_conf = Some(path);
    }
    Ok(Settings {
        portsdir,
        options_dir,
        make_conf,
        make_conf_sources,
        conf_hash,
        env: QueryEnv::Host,
        blacklist: Blacklist::default(),
        notes,
        overlays: Vec::new(),
        flavor_default_all: false,
        framework: Vec::new(),
        options_place: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{resolve_host, OverlaySpec};
    use crate::config::testutil::*;

    /// A local build reads its make.conf in place; --overlay-dir comes first
    /// through a small make.conf that includes it, and an overlay that
    /// make.conf sets itself follows, unnamed and noted.
    #[test]
    fn local_overlay_dirs_go_before_the_make_confs_own() {
        let tmp = tempfile::tempdir().unwrap();
        let ports = tmp.path().join("ports");
        fake_tree(&ports);
        let stage = tmp.path().join("stage");
        fs::create_dir_all(&stage).unwrap();
        let (mine, theirs) = (tmp.path().join("mine"), tmp.path().join("theirs"));
        fs::create_dir_all(&mine).unwrap();
        fs::create_dir_all(&theirs).unwrap();
        let etc = tmp.path().join("etc");
        fs::create_dir_all(&etc).unwrap();
        fs::write(etc.join("make.conf"), ".include \"overlays.conf\"\n").unwrap();
        fs::write(etc.join("overlays.conf"), format!("OVERLAYS+={}\n", theirs.display())).unwrap();
        let absent = tmp.path().join("no-poudriere.d");
        let conf = etc.join("make.conf");
        let host = HostPaths { make_conf: &conf, ..host(tmp.path(), &absent, &ports, tmp.path()) };
        let specs = [OverlaySpec::Dir(mine.clone())];

        let s = resolve_host(&host, &Request { overlays: &specs, ..Request::default() }, &stage).unwrap();
        let paths: Vec<_> = s.overlays.iter().map(|o| o.path.clone()).collect();
        assert_eq!(paths, [mine.canonicalize().unwrap(), theirs.canonicalize().unwrap()]);
        assert!(s.notes.iter().any(|n| n.contains("set by make.conf")), "{:?}", s.notes);
        let plain = resolve_host(&host, &Request::default(), &stage).unwrap();
        assert_eq!(plain.make_conf.as_deref(), Some(conf.as_path()), "read in place without one");
        assert_ne!(s.conf_hash, plain.conf_hash);

        // A make.conf that assigns rather than appends keeps --overlay-dir.
        fs::write(etc.join("overlays.conf"), format!("OVERLAYS={}\n", theirs.display())).unwrap();
        let s = resolve_host(&host, &Request { overlays: &specs, ..Request::default() }, &stage).unwrap();
        let paths: Vec<_> = s.overlays.iter().map(|o| o.path.clone()).collect();
        assert_eq!(paths, [mine.canonicalize().unwrap(), theirs.canonicalize().unwrap()]);
    }

    /// A local build's overlays, in make's order: --overlay-dir, then the
    /// environment's (absolute), then make.conf's own; the environment's
    /// reach make as make.conf lines, not through the queries' environment.
    #[test]
    fn local_overlay_dirs_come_before_the_environments() {
        let tmp = tempfile::tempdir().unwrap();
        let ports = tmp.path().join("ports");
        fake_tree(&ports);
        let stage = tmp.path().join("stage");
        fs::create_dir_all(&stage).unwrap();
        let (mine, env) = (tmp.path().join("mine"), tmp.path().join("env"));
        fs::create_dir_all(&mine).unwrap();
        fs::create_dir_all(&env).unwrap();
        let absent = tmp.path().join("no-poudriere.d");
        let envs = [env.clone()];
        let host = HostPaths { env_overlays: &envs, ..host(tmp.path(), &absent, &ports, tmp.path()) };
        let specs = [OverlaySpec::Dir(mine.clone())];
        let s = resolve_host(&host, &Request { overlays: &specs, ..Request::default() }, &stage).unwrap();
        let paths: Vec<_> = s.overlays.iter().map(|o| o.path.clone()).collect();
        assert_eq!(paths, [mine.canonicalize().unwrap(), env.canonicalize().unwrap()]);
        // Without --overlay-dir the environment's alone, through make.conf.
        let s = resolve_host(&host, &Request::default(), &stage).unwrap();
        let conf = fs::read_to_string(s.make_conf.unwrap()).unwrap();
        assert!(conf.contains(&env.display().to_string()), "{conf}");
    }

    /// make's PORTSDIR and PORT_DBDIR are a local build's defaults, and the
    /// banner says where they came from.
    #[test]
    fn local_defaults_from_make_are_noted() {
        let tmp = tempfile::tempdir().unwrap();
        let ports = tmp.path().join("ports");
        fake_tree(&ports);
        let db = tmp.path().join("db");
        let stage = tmp.path().join("stage");
        fs::create_dir_all(&stage).unwrap();
        let absent = tmp.path().join("no-poudriere.d");
        let host = HostPaths {
            ports_from_make: true,
            db_from_make: true,
            ..host(tmp.path(), &absent, &ports, &db)
        };
        let s = resolve_host(&host, &Request::default(), &stage).unwrap();
        assert_eq!((s.portsdir.as_path(), s.options_dir.as_path()), (ports.as_path(), db.as_path()));
        assert!(s.notes.iter().any(|n| n.ends_with("from PORTSDIR")), "{:?}", s.notes);
        assert!(s.notes.iter().any(|n| n.ends_with("from PORT_DBDIR")), "{:?}", s.notes);
        // -P and -o win, and then there is nothing to note.
        let other = tmp.path().join("other");
        fake_tree(&other);
        let req = Request { portsdir: Some(&other), options_dir: Some(&other), ..Request::default() };
        let s = resolve_host(&host, &req, &stage).unwrap();
        assert_eq!((s.portsdir.as_path(), s.options_dir.as_path()), (other.as_path(), other.as_path()));
        assert!(s.notes.is_empty(), "{:?}", s.notes);
    }

    /// -l ignores a poudriere.d that exists: the host's tree, options dir,
    /// environment, and no poudriere layer or blacklist.
    #[test]
    fn local_ignores_poudriere_d() {
        let tmp = tempfile::tempdir().unwrap();
        let ports = tmp.path().join("ports");
        fake_tree(&ports);
        let db = tmp.path().join("db");
        let stage = tmp.path().join("stage");
        fs::create_dir_all(&stage).unwrap();
        let pd = tmp.path().join("poudriere.d");
        fs::create_dir_all(pd.join("options")).unwrap();
        fs::write(pd.join("make.conf"), "WITH_DEBUG=yes\n").unwrap();
        fs::write(pd.join("blacklist"), "www/nginx\n").unwrap();
        let host = host(tmp.path(), &pd, &ports, &db);

        let local = Request { local: true, ..Request::default() };
        let s = resolve_host(&host, &local, &stage).unwrap();
        assert_eq!((s.portsdir.as_path(), s.options_dir.as_path()), (ports.as_path(), db.as_path()));
        assert_eq!(s.env, QueryEnv::Host);
        assert_eq!(s.make_conf.as_deref(), Some(Path::new("/dev/null")), "none to read, not /etc/make.conf");
        assert!(s.make_conf_sources.is_empty());
        assert!(s.blacklist.is_empty());
        // The host's make.conf, not poudriere.d's, staged and keyed.
        let etc = tmp.path().join("etc-make.conf");
        fs::write(&etc, "DEFAULT_VERSIONS+=ssl=base\n").unwrap();
        let s = resolve_host(&HostPaths { make_conf: &etc, ..host }, &local, &stage).unwrap();
        assert_eq!(s.make_conf_sources, vec![etc.clone()]);
        // Read in place, for its relative .includes, and keyed all the same.
        assert_eq!(s.make_conf.as_ref(), Some(&etc));
        let (_, empty_hash, _) = stage_make_conf(String::new(), &[], "", &stage).unwrap();
        assert_ne!(s.conf_hash, empty_hash);
        // A make.conf make reads whatever its bytes, a Latin-1 comment
        // included, is read and keyed, not taken for none; one that exists
        // but cannot be read stops the run, as it would stop make.
        fs::write(&etc, b"# r\xe9glages\nDEFAULT_VERSIONS+=ssl=base\n").unwrap();
        let s = resolve_host(&HostPaths { make_conf: &etc, ..host }, &local, &stage).unwrap();
        assert_eq!((s.make_conf.as_ref(), s.make_conf_sources.as_slice()), (Some(&etc), [etc.clone()].as_slice()));
        if !nix_is_root() {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&etc, fs::Permissions::from_mode(0o000)).unwrap();
            let err = resolve_host(&HostPaths { make_conf: &etc, ..host }, &local, &stage).unwrap_err();
            assert!(format!("{err:#}").contains("etc-make.conf"), "{err:#}");
            fs::set_permissions(&etc, fs::Permissions::from_mode(0o644)).unwrap();
        }
        // The same host without -l is a poudriere build.
        let s = resolve_host(&host, &Request::default(), &stage).unwrap();
        assert_eq!(s.options_dir, pd.join("options"));
        assert!(!s.blacklist.is_empty());
    }
}
