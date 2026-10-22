//! ESP32-S3 eight-bit DVP camera receiver on the LCD_CAM controller.
//!
//! One capture is one sensor frame. The receiver waits for the frame
//! boundary on VSYNC, starts the DMA transfer while the sensor is still in
//! vertical blanking, and ends it when the destination is full or at the next
//! VSYNC pulse, whichever comes first. A raw frame (RGB565, YUV422,
//! grayscale) therefore fills exactly its byte count, and a JPEG frame ends
//! at the frame's own boundary.
//!
//! The VSYNC pulse is taken as active high with the frame starting at its
//! falling edge, as Espressif's esp32-camera does for every sensor it drives
//! (`cam_hal.c` sets `vsync_invert` and its ESP32 port waits for the falling
//! edge, `ll_cam_set_pin`). Sensor implementations configure their sensor's
//! VSYNC that way.
//!
//! The DMA storage comes from the Board's bulk memory (PSRAM where the Board
//! has it, which the ESP32-S3's GDMA reaches), sized by the Board's
//! `dma-buffer-bytes`, so a full frame never occupies internal RAM.
//!
//! The receiver busy-waits on VSYNC and on the transfer, at most one frame
//! period each, as esp-hal's own `CameraTransfer::wait` does: a frame boundary
//! is missed by the microsecond, which an executor tick cannot guarantee.

use barracuda_bulk_memory::BulkBox;
use barracuda_peripheral::camera::{FrameReceiver, FrameReceiverErrorType};
use esp_hal::{
    dma::{aligned::DmaAlignedMut, DmaBufError, DmaDescriptor, DmaError, DmaRxBuf},
    gpio::interconnect::{InputSignal, PeripheralInput, PeripheralOutput},
    lcd_cam::{
        cam::{
            Camera as VendorCamera, Config as VendorCameraConfig,
            ConfigError as VendorCameraConfigError,
        },
        CamDmaRxChannel, LcdCam,
    },
    peripherals::LCD_CAM,
    time::{Duration, Instant, Rate},
};

/// Address and length alignment of the DMA storage: the largest data-cache
/// line of the ESP32-S3's external memory, so DMA into PSRAM never shares a
/// cache line with other data and burst sizes always divide a transfer.
pub const CAMERA_DMA_ALIGNMENT: usize = 64;

/// The longest one capture waits for each frame boundary and for the frame:
/// a sensor below one frame per second is not streaming.
const FRAME_TIMEOUT: Duration = Duration::from_millis(1000);

/// Time the controller gets, once the frame-ending VSYNC pulse is over, to
/// pass its end-of-frame mark and the last FIFO bytes to the DMA engine.
/// The next frame's first line is many line periods away.
const FRAME_END_SETTLE: Duration = Duration::from_micros(20);

/// Statically allocates the DMA descriptors of one ESP32-S3 DVP receiver and
/// takes its frame storage from bulk memory, at the generated Board call
/// site.
#[macro_export]
macro_rules! __barracuda_esp32s3_camera_dma_buffer {
    ($bytes:expr) => {
        $crate::hal::camera_dma_buffer_from_bulk_memory(
            $crate::hal::__vendor::dma_descriptors_impl!(
                ($bytes) + $crate::hal::CAMERA_DMA_ALIGNMENT,
                $crate::hal::__vendor::dma::BurstConfig::DEFAULT.max_compatible_chunk_size()
            ),
            $bytes,
        )
    };
}

/// ESP32-S3 camera receiver construction failure.
#[derive(Debug)]
pub enum CameraConfigError {
    /// The Board-selected DMA storage cannot be used by the controller.
    DmaBuffer(DmaBufError),
    /// Bulk memory had no room for the Board-selected frame storage.
    Allocation,
    /// The requested camera clock or signal configuration is unsupported.
    Peripheral(VendorCameraConfigError),
}

impl core::fmt::Display for CameraConfigError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::DmaBuffer(_) => formatter.write_str("invalid camera DMA buffer"),
            Self::Allocation => formatter.write_str("no memory for the camera DMA buffer"),
            Self::Peripheral(_) => formatter.write_str("invalid ESP32-S3 camera configuration"),
        }
    }
}

impl core::error::Error for CameraConfigError {}

/// ESP32-S3 frame-capture failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CameraCaptureError {
    /// A capture is already in progress or the receiver lost ownership state.
    Busy,
    /// The destination holds less than one DMA-aligned block.
    EmptyBuffer,
    /// No VSYNC pulse or no frame end arrived within one second.
    Timeout,
    /// The ESP32-S3 DMA engine rejected or aborted the transfer.
    Dma(DmaError),
}

impl core::fmt::Display for CameraCaptureError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Busy => formatter.write_str("camera receiver is busy"),
            Self::EmptyBuffer => formatter.write_str("camera destination buffer is too small"),
            Self::Timeout => formatter.write_str("camera sent no frame"),
            Self::Dma(_) => formatter.write_str("camera DMA transfer failed"),
        }
    }
}

impl core::error::Error for CameraCaptureError {}

/// ESP32-S3 LCD_CAM receiver, its DMA storage, and the VSYNC level it
/// synchronizes on.
pub struct CameraReceiver {
    camera: Option<VendorCamera<'static>>,
    buffer: Option<DmaRxBuf>,
    vsync: InputSignal<'static>,
}

impl CameraReceiver {
    /// Busy-waits until VSYNC reads `high`, or fails once `started` is one
    /// frame timeout old.
    fn wait_for_vsync(&self, high: bool, started: Instant) -> Result<(), CameraCaptureError> {
        while self.vsync.is_input_high() != high {
            if started.elapsed() > FRAME_TIMEOUT {
                return Err(CameraCaptureError::Timeout);
            }
        }
        Ok(())
    }

    /// Waits for the start of the next frame: the end of a VSYNC pulse.
    fn wait_for_frame_start(&self) -> Result<(), CameraCaptureError> {
        let started = Instant::now();
        // Seen mid-pulse, the pulse's end is still a frame start.
        self.wait_for_vsync(true, started)?;
        self.wait_for_vsync(false, started)
    }
}

impl FrameReceiverErrorType for CameraReceiver {
    type Error = CameraCaptureError;
}

impl FrameReceiver for CameraReceiver {
    async fn receive(&mut self, frame: &mut [u8]) -> Result<usize, Self::Error> {
        let Some(mut buffer) = self.buffer.take() else {
            return Err(CameraCaptureError::Busy);
        };
        let length =
            frame.len().min(buffer.capacity()) / CAMERA_DMA_ALIGNMENT * CAMERA_DMA_ALIGNMENT;
        if length == 0 {
            self.buffer = Some(buffer);
            return Err(CameraCaptureError::EmptyBuffer);
        }
        let Some(camera) = self.camera.take() else {
            self.buffer = Some(buffer);
            return Err(CameraCaptureError::Busy);
        };
        buffer.set_length(length);

        if let Err(error) = self.wait_for_frame_start() {
            self.camera = Some(camera);
            self.buffer = Some(buffer);
            return Err(error);
        }
        let transfer = match camera.receive(buffer) {
            Ok(transfer) => transfer,
            Err((error, camera, buffer)) => {
                self.camera = Some(camera);
                self.buffer = Some(buffer);
                return Err(CameraCaptureError::Dma(error));
            }
        };
        let started = Instant::now();
        let (result, camera, buffer) = loop {
            // The controller stops itself once the destination is full.
            if transfer.is_done() {
                let (result, camera, buffer) = transfer.wait();
                break (result.map_err(CameraCaptureError::Dma), camera, buffer);
            }
            // The next VSYNC pulse ends a frame shorter than the destination.
            if self.vsync.is_input_high() {
                let ended = self.wait_for_vsync(false, started);
                let settle = Instant::now();
                while settle.elapsed() < FRAME_END_SETTLE {}
                let (camera, buffer) = transfer.stop();
                break (ended, camera, buffer);
            }
            if started.elapsed() > FRAME_TIMEOUT {
                let (camera, buffer) = transfer.stop();
                break (Err(CameraCaptureError::Timeout), camera, buffer);
            }
        };
        self.camera = Some(camera);
        let received = buffer.number_of_received_bytes().min(frame.len());
        let copied = buffer.read_received_data(&mut frame[..received]);
        self.buffer = Some(buffer);
        result?;
        Ok(copied)
    }
}

/// Builds DMA storage for `bytes` of frame data from bulk memory, aligned for
/// DMA into PSRAM, behind the statically allocated `descriptors`.
///
/// # Errors
///
/// Returns [`CameraConfigError::Allocation`] when bulk memory has no room and
/// [`CameraConfigError::DmaBuffer`] when the DMA engine cannot use the storage.
pub fn camera_dma_buffer_from_bulk_memory(
    descriptors: DmaAlignedMut<'static, [DmaDescriptor]>,
    bytes: usize,
) -> Result<DmaRxBuf, CameraConfigError> {
    let length = bytes
        .checked_next_multiple_of(CAMERA_DMA_ALIGNMENT)
        .ok_or(CameraConfigError::Allocation)?;
    let room = length
        .checked_add(CAMERA_DMA_ALIGNMENT)
        .ok_or(CameraConfigError::Allocation)?;
    // The Board keeps its receiver for the program's lifetime.
    let storage = BulkBox::<[u8]>::try_new_zeroed_slice(room)
        .map_err(|_| CameraConfigError::Allocation)?
        .leak();
    let offset = storage.as_ptr().align_offset(CAMERA_DMA_ALIGNMENT);
    let end = offset
        .checked_add(length)
        .ok_or(CameraConfigError::Allocation)?;
    let aligned = storage
        .get_mut(offset..end)
        .ok_or(CameraConfigError::Allocation)?;
    let aligned = DmaAlignedMut::new(aligned).map_err(CameraConfigError::DmaBuffer)?;
    DmaRxBuf::new(descriptors, aligned).map_err(CameraConfigError::DmaBuffer)
}

/// Constructs the ESP32-S3 DVP receiver for a sensor the chip clocks.
///
/// # Errors
///
/// Returns [`CameraConfigError::Peripheral`] when the master clock cannot be
/// derived.
#[allow(clippy::too_many_arguments)]
pub fn camera_capture(
    controller: LCD_CAM<'static>,
    dma: impl CamDmaRxChannel<'static>,
    xclk: impl PeripheralOutput<'static>,
    pclk: impl PeripheralInput<'static>,
    vsync: impl PeripheralInput<'static>,
    href: impl PeripheralInput<'static>,
    data0: impl PeripheralInput<'static>,
    data1: impl PeripheralInput<'static>,
    data2: impl PeripheralInput<'static>,
    data3: impl PeripheralInput<'static>,
    data4: impl PeripheralInput<'static>,
    data5: impl PeripheralInput<'static>,
    data6: impl PeripheralInput<'static>,
    data7: impl PeripheralInput<'static>,
    xclk_frequency_hz: u32,
    buffer: DmaRxBuf,
) -> Result<CameraReceiver, CameraConfigError> {
    let vsync: InputSignal<'static> = vsync.into();
    let camera = VendorCamera::new(
        LcdCam::new(controller).cam,
        dma,
        VendorCameraConfig::default().with_frequency(Rate::from_hz(xclk_frequency_hz)),
    )
    .map_err(CameraConfigError::Peripheral)?
    .with_master_clock(xclk);
    Ok(receiver(
        camera,
        [pclk.into(), vsync, href.into()],
        [
            data0.into(),
            data1.into(),
            data2.into(),
            data3.into(),
            data4.into(),
            data5.into(),
            data6.into(),
            data7.into(),
        ],
        buffer,
    ))
}

/// Constructs the ESP32-S3 DVP receiver for a sensor with its own oscillator,
/// such as the M5Stack CoreS3's GC0308: the controller drives no master
/// clock.
///
/// # Errors
///
/// Returns [`CameraConfigError::Peripheral`] when the controller rejects its
/// default configuration.
#[allow(clippy::too_many_arguments)]
pub fn camera_capture_without_master_clock(
    controller: LCD_CAM<'static>,
    dma: impl CamDmaRxChannel<'static>,
    pclk: impl PeripheralInput<'static>,
    vsync: impl PeripheralInput<'static>,
    href: impl PeripheralInput<'static>,
    data0: impl PeripheralInput<'static>,
    data1: impl PeripheralInput<'static>,
    data2: impl PeripheralInput<'static>,
    data3: impl PeripheralInput<'static>,
    data4: impl PeripheralInput<'static>,
    data5: impl PeripheralInput<'static>,
    data6: impl PeripheralInput<'static>,
    data7: impl PeripheralInput<'static>,
    buffer: DmaRxBuf,
) -> Result<CameraReceiver, CameraConfigError> {
    let camera = VendorCamera::new(
        LcdCam::new(controller).cam,
        dma,
        VendorCameraConfig::default(),
    )
    .map_err(CameraConfigError::Peripheral)?;
    Ok(receiver(
        camera,
        [pclk.into(), vsync.into(), href.into()],
        [
            data0.into(),
            data1.into(),
            data2.into(),
            data3.into(),
            data4.into(),
            data5.into(),
            data6.into(),
            data7.into(),
        ],
        buffer,
    ))
}

/// Routes the synchronization and data lines and keeps a view of VSYNC.
fn receiver(
    camera: VendorCamera<'static>,
    [pclk, vsync, href]: [InputSignal<'static>; 3],
    [data0, data1, data2, data3, data4, data5, data6, data7]: [InputSignal<'static>; 8],
    buffer: DmaRxBuf,
) -> CameraReceiver {
    // The controller and this receiver both read the VSYNC pad: the GPIO
    // matrix fans one input out to any number of readers.
    let level = vsync.clone();
    let camera = camera
        .with_pixel_clock(pclk)
        .with_vsync(vsync)
        .with_h_enable(href)
        .with_data0(data0)
        .with_data1(data1)
        .with_data2(data2)
        .with_data3(data3)
        .with_data4(data4)
        .with_data5(data5)
        .with_data6(data6)
        .with_data7(data7);
    CameraReceiver {
        camera: Some(camera),
        buffer: Some(buffer),
        vsync: level,
    }
}
