use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// Kind of an options group, with the constraint it imposes on its members.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GroupKind {
    /// Free choice.
    Group,
    /// At least one member must be selected.
    Multi,
    /// Exactly one member must be selected.
    Single,
    /// At most one member may be selected.
    Radio,
}

impl GroupKind {
    pub fn label(self) -> &'static str {
        match self {
            GroupKind::Group => "group",
            GroupKind::Multi => "multi (pick at least one)",
            GroupKind::Single => "single (pick exactly one)",
            GroupKind::Radio => "radio (pick at most one)",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OptionGroup {
    pub kind: GroupKind,
    pub name: String,
    pub desc: String,
    pub members: Vec<String>,
}

/// Per-option metadata gathered from the ports framework.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct OptionDef {
    pub desc: String,
    /// Options force-enabled when this one is on (`<OPT>_IMPLIES`).
    pub implies: Vec<String>,
    /// Options incompatible with this one (`<OPT>_PREVENTS`).
    pub prevents: Vec<String>,
    pub prevents_msg: Option<String>,
    /// Enabling this option marks the port BROKEN with this message.
    pub broken: Option<String>,
    /// Enabling this option marks the port IGNOREd with this message.
    pub ignore: Option<String>,
}

/// Everything the ports framework reports about a port's options,
/// including the make.conf layers that feed into the final PORT_OPTIONS.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PortOptions {
    /// COMPLETE_OPTIONS_LIST, in the framework's order (display order).
    pub complete: Vec<String>,
    /// OPTIONS_DEFAULT (intersected with complete at display time).
    pub defaults: BTreeSet<String>,
    /// Final effective set as make computed it (PORT_OPTIONS).
    pub effective: BTreeSet<String>,
    pub groups: Vec<OptionGroup>,
    pub defs: BTreeMap<String, OptionDef>,
    /// Global make.conf OPTIONS_SET / OPTIONS_UNSET (filtered to complete).
    pub mc_set: BTreeSet<String>,
    pub mc_unset: BTreeSet<String>,
    /// Per-port make.conf ${OPTIONS_NAME}_SET / _UNSET.
    pub port_set: BTreeSet<String>,
    pub port_unset: BTreeSet<String>,
    /// All *_FORCE knobs merged (these override the options file).
    pub force_set: BTreeSet<String>,
    pub force_unset: BTreeSet<String>,
}

/// Where an option's current value comes from, following the
/// bsd.options.mk precedence chain (lowest to highest).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provenance {
    Default,
    MakeConfGlobal,
    MakeConfPort,
    File,
    Forced,
}

impl PortOptions {
    pub fn has_options(&self) -> bool {
        !self.complete.is_empty()
    }

    /// Whether toggling this option in the options file can have any effect.
    pub fn is_forced(&self, opt: &str) -> bool {
        self.force_set.contains(opt) || self.force_unset.contains(opt)
    }

    /// Provenance of the option's value given the enabled set recorded in the
    /// options file (None = no file entry for it).
    pub fn provenance(&self, opt: &str, file_state: Option<bool>) -> Provenance {
        if self.is_forced(opt) {
            Provenance::Forced
        } else if file_state.is_some() {
            Provenance::File
        } else if self.port_set.contains(opt) || self.port_unset.contains(opt) {
            Provenance::MakeConfPort
        } else if self.mc_set.contains(opt) || self.mc_unset.contains(opt) {
            Provenance::MakeConfGlobal
        } else {
            Provenance::Default
        }
    }
}
