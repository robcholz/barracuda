use embedded_storage::nor_flash::{NorFlash, NorFlashError as _, ReadNorFlash};

macro_rules! native_storage_contract {
    ($test:ident, $platform:path) => {
        #[test]
        fn $test() {
            use $platform::{
                FileLayout, FileLayoutError, FileNorFlash, FileNorFlashError, FileRegion,
                FileRegionAccess, VolatileNorFlash, VolatileNorFlashError,
            };

            let directory = tempfile::tempdir().expect("create native flash directory");
            let image = directory.path().join("nested").join("board.flash");
            let mut flash = FileNorFlash::open(&image, 8192).expect("create native flash image");
            assert_eq!(flash.capacity(), 8192);
            let mut erased = [0_u8; 4];
            flash.read(32, &mut erased).expect("read erased file flash");
            assert_eq!(erased, [0xff; 4]);

            flash.write(32, &[0x0f, 0xf0]).expect("program file flash");
            drop(flash);
            let mut flash = FileNorFlash::open(&image, 8192).expect("reopen native flash image");
            let mut persisted = [0_u8; 2];
            flash
                .read(32, &mut persisted)
                .expect("read persisted bytes");
            assert_eq!(persisted, [0x0f, 0xf0]);
            flash.erase(0, 4096).expect("erase aligned file sector");
            flash.read(32, &mut persisted).expect("read erased sector");
            assert_eq!(persisted, [0xff; 2]);

            assert!(matches!(
                FileNorFlash::open(&image, 4096),
                Err(FileNorFlashError::CapacityMismatch {
                    expected: 4096,
                    actual: 8192
                })
            ));
            assert!(matches!(
                FileNorFlash::open(directory.path(), 4096),
                Err(FileNorFlashError::Io(_))
            ));
            assert!(matches!(
                flash.read(8191, &mut [0_u8; 2]),
                Err(FileNorFlashError::OutOfBounds)
            ));
            assert!(matches!(
                flash.erase(1, 4097),
                Err(FileNorFlashError::NotAligned)
            ));
            assert!(matches!(
                flash.erase(4096, 0),
                Err(FileNorFlashError::OutOfBounds)
            ));
            assert_eq!(
                FileNorFlashError::OutOfBounds.kind(),
                embedded_storage::nor_flash::NorFlashErrorKind::OutOfBounds
            );
            assert_eq!(
                FileNorFlashError::NotAligned.kind(),
                embedded_storage::nor_flash::NorFlashErrorKind::NotAligned
            );

            let mut volatile = VolatileNorFlash::new(8192);
            assert_eq!(volatile.capacity(), 8192);
            volatile
                .read(0, &mut erased)
                .expect("read erased volatile flash");
            assert_eq!(erased, [0xff; 4]);
            volatile.write(8, &[0x0f]).expect("first volatile program");
            volatile.write(8, &[0xf0]).expect("second volatile program");
            let mut programmed = [0xff];
            volatile
                .read(8, &mut programmed)
                .expect("read programmed volatile byte");
            assert_eq!(programmed, [0x00], "NOR writes only clear bits");
            volatile.erase(0, 4096).expect("erase volatile sector");
            volatile
                .read(8, &mut programmed)
                .expect("read erased volatile byte");
            assert_eq!(programmed, [0xff]);
            assert!(matches!(
                volatile.read(8192, &mut [0_u8; 1]),
                Err(VolatileNorFlashError::OutOfBounds)
            ));
            assert!(matches!(
                volatile.erase(1, 4097),
                Err(VolatileNorFlashError::NotAligned)
            ));
            assert!(matches!(
                volatile.erase(4096, 0),
                Err(VolatileNorFlashError::OutOfBounds)
            ));
            assert_eq!(
                VolatileNorFlashError::OutOfBounds.kind(),
                embedded_storage::nor_flash::NorFlashErrorKind::OutOfBounds
            );
            assert_eq!(
                VolatileNorFlashError::NotAligned.kind(),
                embedded_storage::nor_flash::NorFlashErrorKind::NotAligned
            );

            let regions = Box::leak(
                vec![
                    FileRegion::read_write("state", 0, 4096),
                    FileRegion::read_only("assets", 4096, 4096),
                ]
                .into_boxed_slice(),
            );
            let layout = FileLayout::new(8192, regions);
            assert_eq!(layout.capacity(), 8192);
            assert_eq!(layout.regions(&volatile).expect("validate layout"), regions);
            let state = layout
                .writable_region(&volatile, "state")
                .expect("resolve writable region");
            assert_eq!(state.name(), "state");
            assert_eq!(state.offset(), 0);
            assert_eq!(state.size(), 4096);
            assert_eq!(state.access(), FileRegionAccess::ReadWrite);
            let assets = layout
                .read_only_region(&volatile, "assets")
                .expect("resolve read-only region");
            assert_eq!(assets.access(), FileRegionAccess::ReadOnly);
            assert_eq!(
                layout.writable_region(&volatile, "assets"),
                Err(FileLayoutError::RegionReadOnly)
            );
            assert_eq!(
                layout.read_only_region(&volatile, "state"),
                Err(FileLayoutError::RegionWritable)
            );
            assert_eq!(
                layout.writable_region(&volatile, "missing"),
                Err(FileLayoutError::RegionMissing)
            );

            let wrong_capacity = FileLayout::new(4096, regions);
            assert_eq!(
                wrong_capacity.regions(&volatile),
                Err(FileLayoutError::Geometry)
            );
            let invalid_regions: &[Vec<FileRegion>] = &[
                vec![FileRegion::read_write("", 0, 4096)],
                vec![FileRegion::read_write("empty", 0, 0)],
                vec![FileRegion::read_write("past-end", 8192, 4096)],
                vec![FileRegion::read_write("unaligned", 1, 4096)],
                vec![
                    FileRegion::read_write("same", 0, 4096),
                    FileRegion::read_only("same", 4096, 4096),
                ],
                vec![
                    FileRegion::read_write("first", 0, 8192),
                    FileRegion::read_only("overlap", 4096, 4096),
                ],
            ];
            for invalid in invalid_regions {
                let invalid = Box::leak(invalid.clone().into_boxed_slice());
                assert_eq!(
                    FileLayout::new(8192, invalid).regions(&volatile),
                    Err(FileLayoutError::Range)
                );
            }
        }
    };
}
