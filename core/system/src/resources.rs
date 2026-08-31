use barracuda_board_hal::BoardHalResources;
use barracuda_platform::{PartitionAccess, Partitions, PlatformResources};
use barracuda_plugin_api::{IntoLuaHardwareResources, LuaHardwareResources};
use barracuda_target_api::TargetResources;
use embassy_net::Stack;

const SYSTEM_PARTITION: &str = "system";
const KV_DATABASE_PARTITION: &str = "kv_database";
const WEB_ASSETS_PARTITION: &str = "web_assets";

pub(super) struct PreparedPartitions<Region, const P: usize> {
    pub(super) system: Region,
    pub(super) kv_database: Region,
    pub(super) web_assets: Region,
    pub(super) remaining: Partitions<Region, P>,
}

pub(super) struct PreparedTarget<Region, Tls, BoardHal, const P: usize> {
    pub(super) ip_stack: Stack<'static>,
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

pub(super) fn prepare<Region, Tls, BoardHal, const P: usize>(
    resources: TargetResources<PlatformResources<Tls, Partitions<Region, P>>, BoardHal>,
) -> Result<PreparedTarget<Region, Tls, BoardHal, P>, SystemResourceError> {
    let TargetResources {
        platform:
            PlatformResources {
                ip_stack,
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
    let web_assets = take_partition(
        &mut partitions,
        WEB_ASSETS_PARTITION,
        PartitionAccess::ReadOnly,
    )?;

    Ok(PreparedTarget {
        ip_stack,
        tls,
        partitions: PreparedPartitions {
            system,
            kv_database,
            web_assets,
            remaining: partitions,
        },
        board_hal,
    })
}

pub(super) fn prepare_board_hal<Builtins, Io>(
    resources: BoardHalResources<Builtins, Io>,
) -> (Builtins, LuaHardwareResources)
where
    Io: IntoLuaHardwareResources,
{
    (
        resources.builtins,
        resources.io.into_lua_hardware_resources(),
    )
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

    use super::{prepare, prepare_board_hal, SystemResourceError};

    #[derive(Debug, PartialEq, Eq)]
    struct BoardHal(u8);

    fn partition(name: &'static str, access: PartitionAccess, region: u8) -> NamedPartition<u8> {
        NamedPartition::new(name, access, region)
    }

    fn target(
        partitions: Partitions<u8, 4>,
    ) -> TargetResources<PlatformResources<(), Partitions<u8, 4>>, BoardHal> {
        TargetResources {
            platform: PlatformResources {
                ip_stack: never_embassy_stack(),
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
            .insert(partition("web_assets", PartitionAccess::ReadOnly, 3))
            .expect("insert unassigned Web assets partition");
        partitions
    }

    #[test]
    fn prepares_target_resources_without_flattening_platform_and_board_hal() {
        let prepared = prepare(target(complete_partitions())).expect("prepare target resources");

        assert_eq!(prepared.partitions.system, 1);
        assert_eq!(prepared.partitions.kv_database, 2);
        assert_eq!(prepared.partitions.web_assets, 3);
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
    fn moves_exposed_io_into_lua_hardware_resources() {
        let resources =
            barracuda_board_hal::BoardHalResources::new(7_u8, barracuda_board_hal::NoExposedIo);

        let (builtins, mut hardware) = prepare_board_hal(resources);

        assert_eq!(builtins, 7);
        assert!(hardware.take_gpio().is_none());
        assert!(hardware.take_i2c().is_none());
        assert!(hardware.take_spi().is_none());
    }

    #[test]
    fn rejects_each_missing_required_partition() {
        for missing in ["system", "kv_database", "web_assets"] {
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
                "web_assets",
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
