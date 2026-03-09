use anyhow::Result;
use evdev::EventType as EvdevEventType;
use evdev::{Device, EventStream, InputEvent};
use log::{debug, info, trace};

use std::time::Duration;
use tokio::time::sleep;

use crate::cancellation::GhostwriterCancellation;
use crate::device::DeviceModel;
use crate::screenshot::Screenshot;
use crate::simulation::{SimulationConfig, TouchSimulator};

/// The active pen tool slot in the RMPP xochitl palette.
/// These correspond to the first two slots in the pen type grid.
/// Verified palette slot coordinates: Ballpoint=(96,119), Fineliner=(150,119).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PenTool {
    Ballpoint,
    Fineliner,
    Unknown,
}

#[derive(Debug, Clone, Copy)]
pub enum TriggerCorner {
    UpperRight,
    UpperLeft,
    LowerRight,
    LowerLeft,
}

impl TriggerCorner {
    pub fn from_string(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "ur" | "upper-right" => Ok(TriggerCorner::UpperRight),
            "ul" | "upper-left" => Ok(TriggerCorner::UpperLeft),
            "lr" | "lower-right" => Ok(TriggerCorner::LowerRight),
            "ll" | "lower-left" => Ok(TriggerCorner::LowerLeft),
            _ => Err(anyhow::anyhow!(
                "Invalid trigger corner: {}. Use UR, UL, LR, LL, upper-right, upper-left, lower-right, or lower-left",
                s
            )),
        }
    }
}

// Output dimensions remain the same for both devices
const VIRTUAL_WIDTH: u16 = 768;
const VIRTUAL_HEIGHT: u16 = 1024;

// Event codes
const ABS_MT_SLOT: u16 = 47;
const ABS_MT_TOUCH_MAJOR: u16 = 48;
const ABS_MT_TOUCH_MINOR: u16 = 49;
const ABS_MT_ORIENTATION: u16 = 52;
const ABS_MT_POSITION_X: u16 = 53;
const ABS_MT_POSITION_Y: u16 = 54;
// const ABS_MT_TOOL_TYPE: u16 = 55;
const ABS_MT_TRACKING_ID: u16 = 57;
const ABS_MT_PRESSURE: u16 = 58;

pub enum TouchMode {
    Real {
        input_device: Option<Device>,      // For sending touch events
        event_stream: Option<EventStream>, // For reading touch events
        device_model: DeviceModel,
    },
    Simulated {
        simulator: TouchSimulator,
    },
}

pub struct Touch {
    mode: TouchMode,
    trigger_corner: TriggerCorner,
}

impl Touch {
    pub fn new(no_touch: bool, trigger_corner: TriggerCorner) -> Self {
        let device_model = DeviceModel::detect();
        info!("Touch using device model: {}", device_model.name());

        let device_path = match device_model {
            DeviceModel::Remarkable2 => "/dev/input/event2",
            DeviceModel::RemarkablePaperPro => "/dev/input/event3",
            DeviceModel::Unknown => "/dev/input/event2", // Default to RM2
        };

        let (input_device, event_stream) = if no_touch {
            (None, None)
        } else {
            let input_dev = Device::open(device_path).unwrap();
            let read_dev = Device::open(device_path).unwrap();
            let stream = read_dev.into_event_stream().unwrap();
            (Some(input_dev), Some(stream))
        };

        Self {
            mode: TouchMode::Real {
                input_device,
                event_stream,
                device_model,
            },
            trigger_corner,
        }
    }

    pub fn new_simulated(simulation_config: SimulationConfig, trigger_corner: TriggerCorner) -> Result<Self> {
        let simulator = TouchSimulator::new(simulation_config, trigger_corner)?;
        info!("Touch using simulation mode");

        Ok(Self {
            mode: TouchMode::Simulated { simulator },
            trigger_corner,
        })
    }

    pub async fn wait_for_trigger(&mut self, cancellation: &GhostwriterCancellation) -> Result<()> {
        debug!("wait_for_trigger: entered, checking mode");
        match &mut self.mode {
            TouchMode::Simulated { simulator } => {
                debug!("wait_for_trigger: using Simulated mode");
                simulator.wait_for_trigger(cancellation).await
            }
            TouchMode::Real {
                event_stream, device_model, ..
            } => {
                debug!("wait_for_trigger: using Real device mode");
                let trigger_corner = self.trigger_corner;
                Self::wait_for_real_trigger(event_stream, device_model, trigger_corner, cancellation).await
            }
        }
    }

    async fn wait_for_real_trigger(
        event_stream: &mut Option<EventStream>,
        device_model: &DeviceModel,
        trigger_corner: TriggerCorner,
        cancellation: &GhostwriterCancellation,
    ) -> Result<()> {
        debug!("wait_for_real_trigger: entered");
        let mut position_x = 0;
        let mut position_y = 0;

        if let Some(events) = event_stream {
            debug!("wait_for_real_trigger: event stream available, entering wait loop");

            loop {
                debug!("wait_for_real_trigger: loop iteration starting");
                tokio::select! {
                    // Check for cancellation (only main token, not execution cycles)
                    _ = async {
                        while !cancellation.should_cancel_main() {
                            sleep(Duration::from_millis(50)).await;
                        }
                    } => {
                        debug!("wait_for_real_trigger: cancellation detected");
                        debug!("Touch waiting cancelled due to shutdown");
                        return Err(anyhow::anyhow!("Touch waiting cancelled"));
                    }

                    // Wait for next event
                    event_result = events.next_event() => {
                        debug!("wait_for_real_trigger: received event");
                        match event_result {
                            Ok(event) => {
                                if event.code() == ABS_MT_POSITION_X {
                                    position_x = event.value();
                                }
                                if event.code() == ABS_MT_POSITION_Y {
                                    position_y = event.value();
                                }
                                if event.code() == ABS_MT_TRACKING_ID && event.value() == -1 {
                                    let (x, y) = Self::input_to_virtual((position_x, position_y), device_model);
                                    debug!("Touch release detected at ({}, {}) normalized ({}, {})", position_x, position_y, x, y);
                                    if Self::is_in_trigger_zone(x, y, trigger_corner) {
                                        debug!("Touch release in target zone!");
                                        debug!("wait_for_real_trigger: returning Ok()");
                                        return Ok(());
                                    } else {
                                        debug!("Touch release NOT in trigger zone, continuing");
                                    }
                                }
                            }
                            Err(e) => {
                                debug!("Error reading touch events: {}", e);
                                return Err(e.into());
                            }
                        }
                    }
                }
            }
        } else {
            debug!("wait_for_real_trigger: no event stream available, entering cancellation wait loop");
            // No event stream available, just wait for cancellation
            loop {
                if cancellation.should_cancel_main() {
                    debug!("wait_for_real_trigger: cancellation detected in no-stream path");
                    debug!("Touch waiting cancelled due to shutdown");
                    return Err(anyhow::anyhow!("Touch waiting cancelled"));
                }
                sleep(Duration::from_millis(50)).await;
            }
        }
    }

    pub async fn touch_start(&mut self, xy: (i32, i32)) -> Result<()> {
        match &mut self.mode {
            TouchMode::Simulated { .. } => {
                debug!("Simulated touch_start at ({}, {})", xy.0, xy.1);
                Ok(())
            }
            TouchMode::Real {
                input_device, device_model, ..
            } => {
                let (x, y) = Self::virtual_to_input(xy, device_model);
                if let Some(device) = input_device {
                    trace!("touch_start at ({}, {})", x, y);
                    device.send_events(&[
                        InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_SLOT, 0),
                        InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_TRACKING_ID, 1),
                        InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_POSITION_X, x),
                        InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_POSITION_Y, y),
                        InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_PRESSURE, 100),
                        InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_TOUCH_MAJOR, 17),
                        InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_TOUCH_MINOR, 17),
                        InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_ORIENTATION, 4),
                        InputEvent::new(EvdevEventType::SYNCHRONIZATION.0, 0, 0), // SYN_REPORT
                    ])?;
                    sleep(Duration::from_millis(1)).await;
                }
                Ok(())
            }
        }
    }

    pub async fn touch_stop(&mut self) -> Result<()> {
        match &mut self.mode {
            TouchMode::Simulated { .. } => {
                debug!("Simulated touch_stop");
                Ok(())
            }
            TouchMode::Real { input_device, .. } => {
                if let Some(device) = input_device {
                    trace!("touch_stop");
                    device.send_events(&[
                        InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_SLOT, 0),
                        InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_TRACKING_ID, -1),
                        InputEvent::new(EvdevEventType::SYNCHRONIZATION.0, 0, 0), // SYN_REPORT
                    ])?;
                    sleep(Duration::from_millis(1)).await;
                }
                Ok(())
            }
        }
    }

    pub async fn goto_xy(&mut self, xy: (i32, i32)) -> Result<()> {
        match &mut self.mode {
            TouchMode::Simulated { .. } => {
                debug!("Simulated goto_xy at ({}, {})", xy.0, xy.1);
                Ok(())
            }
            TouchMode::Real {
                input_device, device_model, ..
            } => {
                let (x, y) = Self::virtual_to_input(xy, device_model);
                if let Some(device) = input_device {
                    device.send_events(&[
                        InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_SLOT, 0),
                        InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_TRACKING_ID, 1),
                        InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_POSITION_X, x),
                        InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_POSITION_Y, y),
                        InputEvent::new(EvdevEventType::SYNCHRONIZATION.0, 0, 0), // SYN_REPORT
                    ])?;
                }
                Ok(())
            }
        }
    }

    pub async fn tap_middle_bottom(&mut self) -> Result<()> {
        self.touch_start((384, 1023)).await?; // middle bottom
        sleep(Duration::from_millis(100)).await;
        self.touch_stop().await?;
        // sleep(Duration::from_millis(10));
        // sleep(Duration::from_millis(100));
        Ok(())
    }

    // ── Tool palette helpers ────────────────────────────────────────────────

    /// Pixel coordinates used to detect palette/tool state in the 768×1024 virtual space.
    /// Verified on RMPP by pixel analysis of screenshots.
    ///
    /// PALETTE_OPEN_PIXEL (70,100): white=closed, black=open (left edge of palette panel).
    /// TOOL_SIDEBAR_PIXEL (2,77):   white=ballpoint selected, black=fineliner OR palette open.
    /// Combined logic:
    ///   is_palette_open  = PALETTE_OPEN_PIXEL.r < 128
    ///   is_fineliner_sel = TOOL_SIDEBAR_PIXEL.r < 128  &&  !is_palette_open
    ///   is_ballpoint_sel = TOOL_SIDEBAR_PIXEL.r >= 128 &&  !is_palette_open
    const PALETTE_OPEN_PIXEL: (u32, u32) = (70, 100);
    const TOOL_SIDEBAR_PIXEL: (u32, u32) = (2, 77);

    /// Palette slot virtual coordinates (inside the open palette).
    const SLOT_BALLPOINT: (i32, i32) = (96, 119);
    const SLOT_FINELINER: (i32, i32) = (150, 119);
    /// Palette toggle button and canvas dismiss point.
    const PALETTE_BUTTON: (i32, i32) = (35, 35);
    const PALETTE_DISMISS: (i32, i32) = (500, 600);

    /// Take a fresh screenshot and read the two indicator pixels.
    /// Returns (palette_open, tool) where tool is Ballpoint/Fineliner/Unknown.
    async fn read_tool_state(&self) -> (bool, PenTool) {
        let mut ss = match Screenshot::new() {
            Ok(s) => s,
            Err(_) => return (false, PenTool::Unknown),
        };
        if ss.take_screenshot().is_err() {
            return (false, PenTool::Unknown);
        }
        let (px, py) = Self::PALETTE_OPEN_PIXEL;
        let (sx, sy) = Self::TOOL_SIDEBAR_PIXEL;
        let palette_open = ss.get_pixel(px, py).map(|(r, _, _)| r < 128).unwrap_or(false);
        let sidebar_dark = ss.get_pixel(sx, sy).map(|(r, _, _)| r < 128).unwrap_or(false);
        let tool = if palette_open {
            PenTool::Unknown // Can't tell which is active while palette is open
        } else if sidebar_dark {
            PenTool::Fineliner
        } else {
            PenTool::Ballpoint
        };
        info!(
            "read_tool_state: palette_open={}, sidebar_dark={} → {:?}",
            palette_open, sidebar_dark, tool
        );
        (palette_open, tool)
    }

    /// Open the tool palette (tap the palette button). Does nothing if already open.
    async fn open_palette(&mut self) -> Result<()> {
        let (already_open, _) = self.read_tool_state().await;
        if already_open {
            info!("open_palette: already open, skipping");
            return Ok(());
        }
        self.touch_start(Self::PALETTE_BUTTON).await?;
        sleep(Duration::from_millis(100)).await;
        self.touch_stop().await?;
        sleep(Duration::from_millis(500)).await;
        info!("open_palette: opened");
        Ok(())
    }

    /// Close the tool palette (tap canvas). Does nothing if already closed.
    async fn close_palette(&mut self) -> Result<()> {
        let (is_open, _) = self.read_tool_state().await;
        if !is_open {
            info!("close_palette: already closed, skipping");
            return Ok(());
        }
        self.touch_start(Self::PALETTE_DISMISS).await?;
        sleep(Duration::from_millis(100)).await;
        self.touch_stop().await?;
        sleep(Duration::from_millis(300)).await;
        info!("close_palette: closed");
        Ok(())
    }

    /// Select a specific tool slot inside the open palette.
    async fn tap_palette_slot(&mut self, slot: (i32, i32)) -> Result<()> {
        self.touch_start(slot).await?;
        sleep(Duration::from_millis(100)).await;
        self.touch_stop().await?;
        sleep(Duration::from_millis(300)).await;
        Ok(())
    }

    /// Switch to the given pen tool. Returns the previously active tool so caller can restore.
    /// - Opens palette if closed, reads current selection, taps the desired slot, closes palette.
    /// - If the desired tool is already active, returns immediately without opening the palette.
    pub async fn switch_to_tool(&mut self, target: PenTool) -> Result<PenTool> {
        // First read current state without touching the UI
        let (palette_open, current_tool) = self.read_tool_state().await;

        // If already on the target (and palette is closed), nothing to do
        if !palette_open && current_tool == target {
            info!("switch_to_tool({:?}): already active, no change", target);
            return Ok(current_tool);
        }

        // Remember what we're switching from (if palette was open, we don't know the active tool)
        let previous = if palette_open { PenTool::Unknown } else { current_tool };

        // Open palette (it might already be open)
        if !palette_open {
            self.touch_start(Self::PALETTE_BUTTON).await?;
            sleep(Duration::from_millis(100)).await;
            self.touch_stop().await?;
            sleep(Duration::from_millis(500)).await;
        }

        // Tap the desired slot
        let slot = match target {
            PenTool::Ballpoint => Self::SLOT_BALLPOINT,
            PenTool::Fineliner => Self::SLOT_FINELINER,
            PenTool::Unknown => {
                // Nothing sensible to do; close palette and return
                self.touch_start(Self::PALETTE_DISMISS).await?;
                sleep(Duration::from_millis(100)).await;
                self.touch_stop().await?;
                sleep(Duration::from_millis(300)).await;
                return Ok(previous);
            }
        };
        self.tap_palette_slot(slot).await?;

        // Close palette
        self.touch_start(Self::PALETTE_DISMISS).await?;
        sleep(Duration::from_millis(100)).await;
        self.touch_stop().await?;
        sleep(Duration::from_millis(300)).await;

        info!("switch_to_tool: {:?} → {:?}", previous, target);
        Ok(previous)
    }

    /// Convenience: select fineliner, return previous tool for later restoration.
    pub async fn select_fineliner(&mut self) -> Result<PenTool> {
        self.switch_to_tool(PenTool::Fineliner).await
    }

    /// Convenience: restore a previously saved tool (e.g. after drawing is done).
    pub async fn restore_tool(&mut self, previous: PenTool) -> Result<()> {
        if previous == PenTool::Unknown || previous == PenTool::Fineliner {
            return Ok(()); // Nothing to restore or already on fineliner
        }
        self.switch_to_tool(previous).await?;
        Ok(())
    }

    fn is_in_trigger_zone(x: i32, y: i32, trigger_corner: TriggerCorner) -> bool {
        const CORNER_SIZE: i32 = 68; // Size of the trigger zone (68x68 pixels)

        match trigger_corner {
            TriggerCorner::UpperRight => x > VIRTUAL_WIDTH as i32 - CORNER_SIZE && y < CORNER_SIZE,
            TriggerCorner::UpperLeft => x < CORNER_SIZE && y < CORNER_SIZE,
            TriggerCorner::LowerRight => x > VIRTUAL_WIDTH as i32 - CORNER_SIZE && y > VIRTUAL_HEIGHT as i32 - CORNER_SIZE,
            TriggerCorner::LowerLeft => x < CORNER_SIZE && y > VIRTUAL_HEIGHT as i32 - CORNER_SIZE,
        }
    }

    fn virtual_to_input((x, y): (i32, i32), device_model: &DeviceModel) -> (i32, i32) {
        // Swap and normalize the coordinates
        let x_normalized = x as f32 / VIRTUAL_WIDTH as f32;
        let y_normalized = y as f32 / VIRTUAL_HEIGHT as f32;
        let (screen_width, screen_height) = Self::screen_dimensions(device_model);

        match device_model {
            DeviceModel::RemarkablePaperPro => {
                let x_input = (x_normalized * screen_width as f32) as i32;
                let y_input = (y_normalized * screen_height as f32) as i32;
                (x_input, y_input)
            }
            _ => {
                // RM2 coordinate transformation
                let x_input = (x_normalized * screen_width as f32) as i32;
                let y_input = ((1.0 - y_normalized) * screen_height as f32) as i32;
                (x_input, y_input)
            }
        }
    }

    fn input_to_virtual((x, y): (i32, i32), device_model: &DeviceModel) -> (i32, i32) {
        // Swap and normalize the coordinates
        let (screen_width, screen_height) = Self::screen_dimensions(device_model);
        let x_normalized = x as f32 / screen_width as f32;
        let y_normalized = y as f32 / screen_height as f32;

        match device_model {
            DeviceModel::RemarkablePaperPro => {
                let x_input = (x_normalized * VIRTUAL_WIDTH as f32) as i32;
                let y_input = (y_normalized * VIRTUAL_HEIGHT as f32) as i32;
                (x_input, y_input)
            }
            _ => {
                // RM2 coordinate transformation
                let x_input = (x_normalized * VIRTUAL_WIDTH as f32) as i32;
                let y_input = ((1.0 - y_normalized) * VIRTUAL_HEIGHT as f32) as i32;
                (x_input, y_input)
            }
        }
    }

    fn screen_dimensions(device_model: &DeviceModel) -> (u32, u32) {
        match device_model {
            DeviceModel::Remarkable2 => (1404, 1872),
            DeviceModel::RemarkablePaperPro => (2065, 2833),
            DeviceModel::Unknown => (1404, 1872), // Default to RM2
        }
    }

    /// Update the trigger corner (called when config changes)
    pub fn set_trigger_corner(&mut self, new_corner: TriggerCorner) {
        self.trigger_corner = new_corner;
        if let TouchMode::Simulated { simulator } = &mut self.mode {
            simulator.set_trigger_corner(new_corner);
        }
    }

    /// Get handle for manual triggering (for web API in simulation mode)
    pub fn get_manual_trigger_handle(&self) -> Option<std::sync::Arc<std::sync::Mutex<Vec<TriggerCorner>>>> {
        match &self.mode {
            TouchMode::Simulated { simulator } => Some(simulator.get_manual_trigger_handle()),
            TouchMode::Real { .. } => None,
        }
    }

    /// Add a manual trigger (for web API in simulation mode)
    pub fn add_manual_trigger(&self, corner: TriggerCorner) {
        if let TouchMode::Simulated { simulator } = &self.mode {
            simulator.add_manual_trigger(corner);
        }
    }
}
