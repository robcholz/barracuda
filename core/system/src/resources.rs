use barracuda_platform::{PartitionAccess, Partitions, PlatformResources};
use barracuda_target_api::TargetResources;
use embassy_net::Stack;

const FILESYSTEM_PARTITION: &str = "filesystem";
const DATABASE_PARTITION: &str = "database";
const WEB_ASSETS_PARTITION: &str = "web-assets";

pub(super) struct PreparedTarget<Region, BoardHal, const P: usize> {
    pub(super) ip_stack: Stack<'static>,
    pub(super) partitions: Partitions<Region, P>,
    pub(super) filesystem: Region,
    pub(super) database: Region,
    pub(super) web_assets: Region,
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

pub(super) fn prepare<Region, BoardHal, const P: usize>(
    resources: TargetResources<PlatformResources<Partitions<Region, P>>, BoardHal>,
) -> Result<PreparedTarget<Region, BoardHal, P>, SystemResourceError> {
    let TargetResources {
        platform: PlatformResources {
            ip_stack,
            mut partitions,
        },
        board_hal,
    } = resources;
    let filesystem = take_partition(
        &mut partitions,
        FILESYSTEM_PARTITION,
        PartitionAccess::ReadWrite,
    )?;
    let database = take_partition(
        &mut partitions,
        DATABASE_PARTITION,
        PartitionAccess::ReadWrite,
    )?;
    let web_assets = take_partition(
        &mut partitions,
        WEB_ASSETS_PARTITION,
        PartitionAccess::ReadOnly,
    )?;

    Ok(PreparedTarget {
        ip_stack,
        partitions,
        filesystem,
        database,
        web_assets,
        board_hal,
    })
}

fn take_partition<Region, const P: usize>(
    partitions: &mut Partitions<Region, P>,
    name: &'static str,
    expected: PartitionAccess,
) -> Result<Region, SystemResourceError> {
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
    Ok(partition.into_region())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic)]

    use barracuda_platform::{NamedPartition, PartitionAccess, Partitions, PlatformResources};
    use barracuda_platform_test::never_embassy_stack;
    use barracuda_target_api::TargetResources;

    use super::{prepare, SystemResourceError};

    #[derive(Debug, PartialEq, Eq)]
    struct BoardHal(u8);

    fn partition(name: &'static str, access: PartitionAccess, region: u8) -> NamedPartition<u8> {
        NamedPartition::new(name, access, region)
    }

    fn target(
        partitions: Partitions<u8, 4>,
    ) -> TargetResources<PlatformResources<Partitions<u8, 4>>, BoardHal> {
        TargetResources {
            platform: PlatformResources {
                ip_stack: never_embassy_stack(),
                partitions,
            },
            board_hal: BoardHal(7),
        }
    }

    fn complete_partitions() -> Partitions<u8, 4> {
        let mut partitions = Partitions::new();
        partitions
            .insert(partition("database", PartitionAccess::ReadWrite, 2))
            .expect("insert database partition");
        partitions
            .insert(partition("future", PartitionAccess::ReadWrite, 4))
            .expect("insert future partition");
        partitions
            .insert(partition("web-assets", PartitionAccess::ReadOnly, 3))
            .expect("insert Web assets partition");
        partitions
            .insert(partition("filesystem", PartitionAccess::ReadWrite, 1))
            .expect("insert filesystem partition");
        partitions
    }

    #[test]
    fn prepares_target_resources_without_flattening_platform_and_board_hal() {
        let prepared = prepare(target(complete_partitions())).expect("prepare target resources");

        assert_eq!(prepared.filesystem, 1);
        assert_eq!(prepared.database, 2);
        assert_eq!(prepared.web_assets, 3);
        assert_eq!(prepared.board_hal, BoardHal(7));
        assert_eq!(prepared.partitions.len(), 1);
        assert_eq!(
            prepared
                .partitions
                .get("future")
                .map(NamedPartition::region),
            Some(&4)
        );
    }

    #[test]
    fn rejects_each_missing_required_partition() {
        for missing in ["filesystem", "database", "web-assets"] {
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
                "filesystem",
                PartitionAccess::ReadWrite,
                PartitionAccess::ReadOnly,
            ),
            (
                "database",
                PartitionAccess::ReadWrite,
                PartitionAccess::ReadOnly,
            ),
            (
                "web-assets",
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
