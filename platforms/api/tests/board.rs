//! Platform resource-boundary tests.

use barracuda_platform::{NamedPartition, PartitionAccess, Partitions, PlatformResources};

#[test]
fn platform_resources_expose_exact_ip_tls_and_partition_capabilities() {
    fn assert_shape<Tls, Partitions>(resources: PlatformResources<Tls, Partitions>) {
        let PlatformResources {
            ip_stack: _,
            tls: _,
            partitions: _,
        } = resources;
    }

    let _assert_shape = assert_shape::<(), ()>;
}

#[test]
fn partitions_are_an_extensible_named_collection() {
    let mut partitions = Partitions::<u8, 4>::new();
    partitions
        .insert(NamedPartition::new(
            "runtime",
            PartitionAccess::ReadWrite,
            1,
        ))
        .expect("insert runtime partition");
    partitions
        .insert(NamedPartition::new("assets", PartitionAccess::ReadOnly, 2))
        .expect("insert asset partition");
    partitions
        .insert(NamedPartition::new(
            "future-plugin-region",
            PartitionAccess::ReadWrite,
            3,
        ))
        .expect("insert arbitrary partition");

    assert_eq!(partitions.len(), 3);
    assert_eq!(
        partitions.get("assets").map(NamedPartition::access),
        Some(PartitionAccess::ReadOnly)
    );
    assert_eq!(
        partitions
            .take("future-plugin-region")
            .map(NamedPartition::into_region),
        Some(3)
    );
    assert!(partitions.get("future-plugin-region").is_none());
}

#[test]
fn partitions_reject_empty_and_duplicate_native_names() {
    let mut partitions = Partitions::<u8, 2>::new();
    assert!(partitions
        .insert(NamedPartition::new("", PartitionAccess::ReadWrite, 1))
        .is_err());
    partitions
        .insert(NamedPartition::new(
            "native-name",
            PartitionAccess::ReadWrite,
            1,
        ))
        .expect("insert first partition");
    assert!(partitions
        .insert(NamedPartition::new(
            "native-name",
            PartitionAccess::ReadOnly,
            2,
        ))
        .is_err());
}

#[test]
fn platform_api_has_no_business_storage_fields() -> Result<(), std::io::Error> {
    let source = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs"),
    )?;
    for forbidden in [
        "filesystem:",
        "database_region:",
        "web_assets",
        "plugin_partition",
        "type FileSystem",
        "type DatabaseRegion",
    ] {
        assert!(
            !source.contains(forbidden),
            "Platform API contains business storage concept `{forbidden}`"
        );
    }
    Ok(())
}
