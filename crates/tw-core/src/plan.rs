use serde::Serialize;

use crate::machine::Machine;

/// The steps `up` would take, printed by `up --plan` without side effects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SetupPlan {
    pub name: String,
    pub steps: Vec<&'static str>,
}

impl SetupPlan {
    pub fn new(name: &str, machine: Option<&Machine>) -> Self {
        let mut steps = Vec::new();
        if machine.is_none_or(|m| !m.installed) {
            steps.push("validate-windows-iso");
            if machine.is_none() {
                steps.push("create-hyper-v-vm");
            } else {
                steps.push("retry-incomplete-installation");
            }
            steps.push("install-windows-unattended");
            steps.push("provision-guest-agent");
        }
        steps.extend(["start-or-resume-vm", "wait-for-guest-agent", "open-terminal-view-if-supported"]);
        Self { name: name.to_string(), steps }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine(installed: bool) -> Machine {
        let mut m = Machine::new("default", "tw-default-3fa9c1d2", Default::default());
        m.installed = installed;
        m
    }

    #[test]
    fn new_vm_plan_installs() {
        let plan = SetupPlan::new("default", None);
        assert_eq!(plan.steps.first(), Some(&"validate-windows-iso"));
        assert!(plan.steps.contains(&"create-hyper-v-vm"));
        assert!(plan.steps.contains(&"provision-guest-agent"));
    }

    #[test]
    fn incomplete_vm_plan_retries() {
        let plan = SetupPlan::new("default", Some(&machine(false)));
        assert!(plan.steps.contains(&"retry-incomplete-installation"));
        assert!(!plan.steps.contains(&"create-hyper-v-vm"));
    }

    #[test]
    fn installed_vm_plan_only_starts() {
        let plan = SetupPlan::new("default", Some(&machine(true)));
        assert_eq!(plan.steps, ["start-or-resume-vm", "wait-for-guest-agent", "open-terminal-view-if-supported"]);
    }
}
