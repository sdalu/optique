/// The wrapper makefile piped to `make -f /dev/stdin optique-config`.
///
/// It includes the port's Makefile (which pulls in the whole ports framework)
/// and dumps everything we need as `.info OPTIQUE|...` lines at parse time,
/// including dynamically-named variables (group members, descriptions) that a
/// plain `make -V` cannot reach generically.
///
/// `DEPENDS` is `_UNIFIED_DEPENDS`; `BUILDDEPS` is the lists poudriere builds
/// without `bulk -t`. On a tree before ports e7679b71cf89 (2026-09-25) the
/// unified list still folds `TEST_DEPENDS` in, and the second one tells
/// those edges apart (see `DepEdge::test_only`); since then the two agree.
///
/// Two hard constraints, both verified against /usr/ports/Mk:
/// - The phony target name must match `*config*`: bsd.port.mk only includes
///   bsd.options.desc.mk (the `<OPT>_DESC` defaults) for such targets.
/// - Every possibly-undefined variable is expanded with `:U` so parsing never
///   aborts on a port that does not define it.
///
/// The per-option records cover the OPTIONS_SLAVE options too: they are not
/// in COMPLETE_OPTIONS_LIST, yet bsd.options.mk:447 puts them in
/// PORT_OPTIONS and pre-check-config (bsd.port.mk ~4987) checks the
/// PREVENTS of every option there.
///
/// OPTIONS_OVERRIDE is dumped only when `defined`, value and all
/// (bsd.options.mk:300-304): an absent line means undefined, an empty one
/// means defined empty — the two make.conf can produce differ, since the
/// latter still forgets the file.
pub const WRAPPER: &str = "\
.include \"${PORTDIR}/Makefile\"
.info OPTIQUE|PKGNAME|${PKGNAME:U}
.info OPTIQUE|FLAVORS|${FLAVORS:U}
.info OPTIQUE|FLAVOR|${FLAVOR:U}
.info OPTIQUE|OPTIONS_NAME|${OPTIONS_NAME:U}
.info OPTIQUE|COMPLETE|${COMPLETE_OPTIONS_LIST:U}
.info OPTIQUE|DEFAULT|${OPTIONS_DEFAULT:U}
.info OPTIQUE|ALL|${ALL_OPTIONS:U}
.info OPTIQUE|PORT_OPTIONS|${PORT_OPTIONS:U}
.if defined(OPTIONS_OVERRIDE)
.info OPTIQUE|OVERRIDE|${OPTIONS_OVERRIDE}
.endif
.info OPTIQUE|DEPENDS|${_UNIFIED_DEPENDS:U}
.info OPTIQUE|BUILDDEPS|${PKG_DEPENDS_ALL:U} ${EXTRACT_DEPENDS_ALL:U} ${PATCH_DEPENDS_ALL:U} ${FETCH_DEPENDS_ALL:U} ${BUILD_DEPENDS_ALL:U} ${LIB_DEPENDS_ALL:U} ${RUN_DEPENDS_ALL:U}
.info OPTIQUE|MC_SET|${OPTIONS_SET:U}
.info OPTIQUE|MC_UNSET|${OPTIONS_UNSET:U}
.info OPTIQUE|PORT_SET|${${OPTIONS_NAME}_SET:U}
.info OPTIQUE|PORT_UNSET|${${OPTIONS_NAME}_UNSET:U}
.info OPTIQUE|FORCE_SET|${OPTIONS_SET_FORCE:U}
.info OPTIQUE|FORCE_UNSET|${OPTIONS_UNSET_FORCE:U}
.info OPTIQUE|PORT_FORCE_SET|${${OPTIONS_NAME}_SET_FORCE:U}
.info OPTIQUE|PORT_FORCE_UNSET|${${OPTIONS_NAME}_UNSET_FORCE:U}
.info OPTIQUE|SLAVE|${OPTIONS_SLAVE:U}
.info OPTIQUE|WITH|${WITH:U}
.info OPTIQUE|WITHOUT|${WITHOUT:U}
.info OPTIQUE|DEFAULT_VERSIONS|${DEFAULT_VERSIONS:U}
.info OPTIQUE|BROKEN|${BROKEN:U}
.info OPTIQUE|IGNORE|${IGNORE:U}
.info OPTIQUE|DEPRECATED|${DEPRECATED:U}
.info OPTIQUE|PKGHELP|${PKGHELP:U}
.info OPTIQUE|COMMENT|${COMMENT:U}
.info OPTIQUE|WWW|${WWW:U}
.info OPTIQUE|DESCR|${DESCR:U}
.for _g in ${OPTIONS_GROUP:U}
.info OPTIQUE|GROUP|${_g}|${OPTIONS_GROUP_${_g}:U}
.endfor
.for _g in ${OPTIONS_SINGLE:U}
.info OPTIQUE|SINGLE|${_g}|${OPTIONS_SINGLE_${_g}:U}
.endfor
.for _g in ${OPTIONS_RADIO:U}
.info OPTIQUE|RADIO|${_g}|${OPTIONS_RADIO_${_g}:U}
.endfor
.for _g in ${OPTIONS_MULTI:U}
.info OPTIQUE|MULTI|${_g}|${OPTIONS_MULTI_${_g}:U}
.endfor
.for _o in ${COMPLETE_OPTIONS_LIST:U} ${OPTIONS_SLAVE:U} ${OPTIONS_GROUP:U} ${OPTIONS_SINGLE:U} ${OPTIONS_RADIO:U} ${OPTIONS_MULTI:U}
.info OPTIQUE|DESC|${_o}|${${_o}_DESC:U}
.  if defined(${_o}_IMPLIES)
.info OPTIQUE|IMPLIES|${_o}|${${_o}_IMPLIES}
.  endif
.  if defined(${_o}_PREVENTS)
.info OPTIQUE|PREVENTS|${_o}|${${_o}_PREVENTS}
.  endif
.  if defined(${_o}_PREVENTS_MSG)
.info OPTIQUE|PREVENTS_MSG|${_o}|${${_o}_PREVENTS_MSG}
.  endif
.  if defined(${_o}_BROKEN)
.info OPTIQUE|OPT_BROKEN|${_o}|${${_o}_BROKEN}
.  endif
.  if defined(${_o}_IGNORE)
.info OPTIQUE|OPT_IGNORE|${_o}|${${_o}_IGNORE}
.  endif
.  if defined(${_o}_LIB_DEPENDS)
.info OPTIQUE|OPT_DEP|${_o}|lib|${${_o}_LIB_DEPENDS}
.  endif
.  if defined(${_o}_RUN_DEPENDS)
.info OPTIQUE|OPT_DEP|${_o}|run|${${_o}_RUN_DEPENDS}
.  endif
.  if defined(${_o}_BUILD_DEPENDS)
.info OPTIQUE|OPT_DEP|${_o}|build|${${_o}_BUILD_DEPENDS}
.  endif
.  if defined(${_o}_FETCH_DEPENDS)
.info OPTIQUE|OPT_DEP|${_o}|fetch|${${_o}_FETCH_DEPENDS}
.  endif
.  if defined(${_o}_EXTRACT_DEPENDS)
.info OPTIQUE|OPT_DEP|${_o}|extract|${${_o}_EXTRACT_DEPENDS}
.  endif
.  if defined(${_o}_PATCH_DEPENDS)
.info OPTIQUE|OPT_DEP|${_o}|patch|${${_o}_PATCH_DEPENDS}
.  endif
.  if defined(${_o}_USES)
.info OPTIQUE|OPT_DEP|${_o}|uses|${${_o}_USES}
.  endif
.endfor
optique-config: .PHONY
\t@true
";

#[cfg(test)]
mod tests {
    use super::WRAPPER;

    /// pre-check-config checks the PREVENTS of every option in PORT_OPTIONS,
    /// OPTIONS_SLAVE ones included: their records are dumped with the
    /// others.
    #[test]
    fn options_slave_options_get_their_records() {
        let for_line = WRAPPER.lines().find(|l| l.starts_with(".for _o in ")).unwrap();
        assert!(for_line.contains("${COMPLETE_OPTIONS_LIST:U} ${OPTIONS_SLAVE:U} "), "{for_line}");
    }

    /// bsd.options.mk:300-304: OPTIONS_OVERRIDE must be dumped only under
    /// `.if defined(...)`, unlike every other tag's bare `:U` expansion, so
    /// an absent line (undefined) and an empty one (defined empty) parse
    /// differently.
    #[test]
    fn override_is_dumped_only_when_defined() {
        let i = WRAPPER.lines().position(|l| l.contains("OPTIQUE|OVERRIDE|")).unwrap();
        let lines: Vec<&str> = WRAPPER.lines().collect();
        assert_eq!(lines[i - 1], ".if defined(OPTIONS_OVERRIDE)", "{}", lines[i - 1]);
        assert_eq!(lines[i], ".info OPTIQUE|OVERRIDE|${OPTIONS_OVERRIDE}");
        assert_eq!(lines[i + 1], ".endif");
    }
}
