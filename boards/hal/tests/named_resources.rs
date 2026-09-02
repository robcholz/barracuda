//! Runtime lookup and move-only ownership behavior of board-exposed resources.

use barracuda_board_hal::{BoardHalResources, NamedResources, ResourceSet};

#[test]
fn named_resources_preserve_identity_mutation_and_empty_board_semantics() {
    let mut pins = NamedResources::new([("status", 1_u8), ("button", 2_u8)]);
    assert!(pins.contains("status"));
    assert!(!pins.contains("missing"));
    assert_eq!(pins.get("status"), Some(&1));
    assert_eq!(pins.get("button"), Some(&2));
    assert_eq!(pins.get("missing"), None);

    *pins.get_mut("status").expect("status pin exists") = 9;
    assert_eq!(pins.get("status"), Some(&9));
    assert_eq!(pins.get("button"), Some(&2));
    assert_eq!(pins.get_mut("missing"), None);

    let empty = NamedResources::<u8, 0>::empty();
    assert!(!empty.contains("anything"));
    assert_eq!(empty.get("anything"), None);

    let resources = BoardHalResources::new("builtins", pins);
    assert_eq!(resources.builtins, "builtins");
    assert_eq!(resources.io.get("status"), Some(&9));
}
