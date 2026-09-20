use barracuda_platform::{
    NamedPartition, PartitionAccess, PartitionFilesystem, Partitions, PlatformResources,
};
use barracuda_target_api::TargetResources;
use embassy_net::Stack;

const SYSTEM_PARTITION: &str = "system";
const KV_DATABASE_PARTITION: &str = "kv_database";
const RESOURCES_PARTITION: &str = "resources";

pub(super) struct PreparedPartitions<Region, const P: usize> {
    pub(super) system: Region,
    pub(super) kv_database: Region,
    pub(super) resources: PreparedFilesystemPartition<Region>,
    pub(super) remaining: Partitions<Region, P>,
}

pub(super) struct PreparedFilesystemPartition<Region> {
    pub(super) region: Region,
    pub(super) filesystem: PartitionFilesystem,
}

pub(super) struct PreparedTarget<Region, Tls, Wifi, BoardHal, const P: usize> {
    pub(super) ip_stack: Stack<'static>,
    pub(super) wifi: Wifi,
    pub(super) tls: Tls,
    pub(super) partitions: PreparedPartitions<Region, P>,
    pub(super) board_hal: BoardHal,
}

/// Failure while assigning selected native partitions to System-owned roles.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SystemResourceError {
    /// The selected native layout does not contain one required System partition.
    #[error("selected target is missing required partition {name}")]
    MissingPartition {
        /// Required native partition name.
        name: &'static str,
    },
    /// A selected native partition has the wrong runtime access discipline.
    #[error("partition {name} has access {actual:?}, but System requires {expected:?}")]
    PartitionAccess {
        /// Required native partition name.
        name: &'static str,
        /// Access discipline required by the assigned System role.
        expected: PartitionAccess,
        /// Access discipline declared by the native layout.
        actual: PartitionAccess,
    },
}

pub(super) fn prepare<Region, Tls, Wifi, BoardHal, const P: usize>(
    resources: TargetResources<PlatformResources<Tls, Partitions<Region, P>, Wifi>, BoardHal>,
) -> Result<PreparedTarget<Region, Tls, Wifi, BoardHal, P>, SystemResourceError> {
    let TargetResources {
        platform:
            PlatformResources {
                ip_stack,
                wifi,
                tls,
                mut partitions,
            },
        board_hal,
    } = resources;
    let system = take_partition(
        &mut partitions,
        SYSTEM_PARTITION,
        PartitionAccess::ReadWrite,
    )?;
    let kv_database = take_partition(
        &mut partitions,
        KV_DATABASE_PARTITION,
        PartitionAccess::ReadWrite,
    )?;
    let resources = take_partition(
        &mut partitions,
        RESOURCES_PARTITION,
        PartitionAccess::ReadOnly,
    )?;

    Ok(PreparedTarget {
        ip_stack,
        wifi,
        tls,
        partitions: PreparedPartitions {
            system: system.into_region(),
            kv_database: kv_database.into_region(),
            resources: PreparedFilesystemPartition {
                filesystem: resources.filesystem(),
                region: resources.into_region(),
            },
            remaining: partitions,
        },
        board_hal,
    })
}

fn take_partition<Region, const P: usize>(
    partitions: &mut Partitions<Region, P>,
    name: &'static str,
    expected: PartitionAccess,
) -> Result<NamedPartition<Region>, SystemResourceError> {
    let partition = partitions
        .take(name)
        .ok_or(SystemResourceError::MissingPartition { name })?;
    let actual = partition.access();
    if actual != expected {
        return Err(SystemResourceError::PartitionAccess {
            name,
            expected,
            actual,
        });
    }
    Ok(partition)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic)]

    use barracuda_platform::{
        NamedPartition, PartitionAccess, PartitionFilesystem, Partitions, PlatformResources,
    };
    use barracuda_platform_test::never_embassy_stack;
    use barracuda_target_api::TargetResources;

    use super::{prepare, SystemResourceError};

    #[derive(Debug, PartialEq, Eq)]
    struct BoardHal(u8);

    fn partition(name: &'static str, access: PartitionAccess, region: u8) -> NamedPartition<u8> {
        let filesystem = match name {
            "system" => PartitionFilesystem::LittleFs,
            "resources" => PartitionFilesystem::FatFs,
            _ => PartitionFilesystem::Raw,
        };
        NamedPartition::new(name, access, filesystem, region)
    }

    fn target(
        partitions: Partitions<u8, 4>,
    ) -> TargetResources<PlatformResources<(), Partitions<u8, 4>, ()>, BoardHal> {
        TargetResources {
            platform: PlatformResources {
                ip_stack: never_embassy_stack(),
                wifi: (),
                tls: (),
                partitions,
            },
            board_hal: BoardHal(7),
        }
    }

    fn complete_partitions() -> Partitions<u8, 4> {
        let mut partitions = Partitions::new();
        partitions
            .insert(partition("kv_database", PartitionAccess::ReadWrite, 2))
            .expect("insert KV database partition");
        partitions
            .insert(partition("future", PartitionAccess::ReadWrite, 4))
            .expect("insert future partition");
        partitions
            .insert(partition("system", PartitionAccess::ReadWrite, 1))
            .expect("insert System partition");
        partitions
            .insert(partition("resources", PartitionAccess::ReadOnly, 3))
            .expect("insert unassigned Plugin resources partition");
        partitions
    }

    #[test]
    fn prepares_target_resources_without_flattening_platform_and_board_hal() {
        let prepared = prepare(target(complete_partitions())).expect("prepare target resources");

        assert_eq!(prepared.partitions.system, 1);
        assert_eq!(prepared.partitions.kv_database, 2);
        assert_eq!(prepared.partitions.resources.region, 3);
        assert_eq!(
            prepared.partitions.resources.filesystem,
            PartitionFilesystem::FatFs
        );
        assert_eq!(prepared.board_hal, BoardHal(7));
        assert_eq!(prepared.partitions.remaining.len(), 1);
        assert_eq!(
            prepared
                .partitions
                .remaining
                .get("future")
                .map(NamedPartition::region),
            Some(&4)
        );
    }

    #[test]
    fn rejects_each_missing_required_partition() {
        for missing in ["system", "kv_database", "resources"] {
            let mut partitions = complete_partitions();
            let _removed = partitions.take(missing);

            let error = match prepare(target(partitions)) {
                Ok(_prepared) => panic!("missing partition must fail"),
                Err(error) => error,
            };
            assert_eq!(
                error,
                SystemResourceError::MissingPartition { name: missing }
            );
        }
    }

    #[test]
    fn rejects_partition_access_that_does_not_match_its_system_role() {
        for (name, expected, actual) in [
            (
                "system",
                PartitionAccess::ReadWrite,
                PartitionAccess::ReadOnly,
            ),
            (
                "kv_database",
                PartitionAccess::ReadWrite,
                PartitionAccess::ReadOnly,
            ),
            (
                "resources",
                PartitionAccess::ReadOnly,
                PartitionAccess::ReadWrite,
            ),
        ] {
            let mut partitions = complete_partitions();
            let original = partitions
                .take(name)
                .expect("required partition exists in test fixture");
            partitions
                .insert(partition(name, actual, *original.region()))
                .expect("replace partition with invalid access");

            let error = match prepare(target(partitions)) {
                Ok(_prepared) => panic!("invalid partition access must fail"),
                Err(error) => error,
            };
            assert_eq!(
                error,
                SystemResourceError::PartitionAccess {
                    name,
                    expected,
                    actual,
                }
            );
        }
    }
}
