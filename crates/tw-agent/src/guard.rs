//! Refuses to run anywhere but inside a Hyper-V guest, so the agent cannot be started on a real desktop by
//! mistake and hand its input and screen to a remote peer.

/// `(SystemManufacturer, SystemProductName)` pairs of supported guests.
const GUEST_SIGNATURES: &[(&str, &str)] =
    &[("Microsoft Corporation", "Virtual Machine"), ("innotek GmbH", "VirtualBox")];

pub fn is_supported_guest(manufacturer: &str, product: &str) -> bool {
    GUEST_SIGNATURES
        .iter()
        .any(|(m, p)| m.eq_ignore_ascii_case(manufacturer.trim()) && p.eq_ignore_ascii_case(product.trim()))
}

#[cfg(windows)]
pub fn bios_value(name: &str) -> Option<String> {
    crate::registry::read_string(r"HARDWARE\DESCRIPTION\System\BIOS", name)
}

#[cfg(not(windows))]
pub fn bios_value(_: &str) -> Option<String> {
    None
}

pub fn check() -> Result<(), String> {
    let manufacturer = bios_value("SystemManufacturer").unwrap_or_default();
    let product = bios_value("SystemProductName").unwrap_or_default();
    if is_supported_guest(&manufacturer, &product) {
        Ok(())
    } else {
        Err(format!(
            "This program runs only inside a terminal-windows virtual machine. This machine reports '{manufacturer}' / '{product}'"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hyper_v_guests_are_accepted() {
        assert!(is_supported_guest("Microsoft Corporation", "Virtual Machine"));
        assert!(is_supported_guest(" microsoft corporation ", "virtual machine"));
        assert!(is_supported_guest("innotek GmbH", "VirtualBox"));
    }

    #[test]
    fn everything_else_is_refused() {
        assert!(!is_supported_guest("Microsoft Corporation", "Surface Laptop 5"));
        assert!(!is_supported_guest("Dell Inc.", "Virtual Machine"));
        assert!(!is_supported_guest("", ""));
        assert!(!is_supported_guest("QEMU", "Standard PC"));
        assert!(!is_supported_guest("innotek GmbH", "Laptop"));
    }

    #[cfg(windows)]
    #[test]
    fn this_development_machine_is_not_a_guest() {
        // Runs on the host that builds the project; if it ever runs inside a guest the check must agree.
        let manufacturer = bios_value("SystemManufacturer").unwrap_or_default();
        let product = bios_value("SystemProductName").unwrap_or_default();
        assert_eq!(check().is_ok(), is_supported_guest(&manufacturer, &product));
    }
}
