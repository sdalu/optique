/// The wrapper makefile piped to `make -f /dev/stdin optique-config`.
///
/// It includes the port's Makefile (which pulls in the whole ports framework)
/// and dumps everything we need as `.info OPTIQUE|...` lines at parse time,
/// including dynamically-named variables (group members, descriptions) that a
/// plain `make -V` cannot reach generically.
///
/// Two hard constraints, both verified against /usr/ports/Mk:
/// - The phony target name must match `*config*`: bsd.port.mk only includes
///   bsd.options.desc.mk (the `<OPT>_DESC` defaults) for such targets.
/// - Every possibly-undefined variable is expanded with `:U` so parsing never
///   aborts on a port that does not define it.
pub const WRAPPER: &str = "\
.include \"${PORTDIR}/Makefile\"
.info OPTIQUE|PKGNAME|${PKGNAME:U}
.info OPTIQUE|FLAVORS|${FLAVORS:U}
.info OPTIQUE|FLAVOR|${FLAVOR:U}
.info OPTIQUE|OPTIONS_NAME|${OPTIONS_NAME:U}
.info OPTIQUE|COMPLETE|${COMPLETE_OPTIONS_LIST:U}
.info OPTIQUE|DEFAULT|${OPTIONS_DEFAULT:U}
.info OPTIQUE|PORT_OPTIONS|${PORT_OPTIONS:U}
.info OPTIQUE|DEPENDS|${_UNIFIED_DEPENDS:U}
.info OPTIQUE|MC_SET|${OPTIONS_SET:U}
.info OPTIQUE|MC_UNSET|${OPTIONS_UNSET:U}
.info OPTIQUE|PORT_SET|${${OPTIONS_NAME}_SET:U}
.info OPTIQUE|PORT_UNSET|${${OPTIONS_NAME}_UNSET:U}
.info OPTIQUE|FORCE_SET|${OPTIONS_SET_FORCE:U} ${${OPTIONS_NAME}_SET_FORCE:U}
.info OPTIQUE|FORCE_UNSET|${OPTIONS_UNSET_FORCE:U} ${${OPTIONS_NAME}_UNSET_FORCE:U}
.info OPTIQUE|DEFAULT_VERSIONS|${DEFAULT_VERSIONS:U}
.info OPTIQUE|BROKEN|${BROKEN:U}
.info OPTIQUE|IGNORE|${IGNORE:U}
.info OPTIQUE|DEPRECATED|${DEPRECATED:U}
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
.for _o in ${COMPLETE_OPTIONS_LIST:U} ${OPTIONS_GROUP:U} ${OPTIONS_SINGLE:U} ${OPTIONS_RADIO:U} ${OPTIONS_MULTI:U}
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
