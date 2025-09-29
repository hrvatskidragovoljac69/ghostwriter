use anyhow::Result;
use evdev::EventType as EvdevEventType;
use evdev::{Device, InputEvent};
use log::{debug, info, trace};

use std::time::Duration;
use tokio::time::{sleep, timeout};

use crate::cancellation::GhostwriterCancellation;
use crate::device::DeviceModel;
use crate::simulation::{SimulationConfig, TouchSimulator};

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
    Real { device: Option<Device>, device_model: DeviceModel },
    Simulated { simulator: TouchSimulator },
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

        let device = if no_touch { None } else { Some(Device::open(device_path).unwrap()) };

        Self {
            mode: TouchMode::Real { device, device_model },
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
        match &mut self.mode {
            TouchMode::Simulated { simulator } => simulator.wait_for_trigger(cancellation).await,
            TouchMode::Real { device, device_model } => {
                let trigger_corner = self.trigger_corner;
                Self::wait_for_real_trigger_static(device, device_model, trigger_corner, cancellation).await
            }
        }
    }

    async fn wait_for_real_trigger_static(
        device: &mut Option<Device>,
        device_model: &DeviceModel,
        trigger_corner: TriggerCorner,
        cancellation: &GhostwriterCancellation,
    ) -> Result<()> {
        let mut position_x = 0;
        let mut position_y = 0;

        loop {
            // Check for cancellation before each iteration
            if cancellation.should_cancel() {
                return Err(anyhow::anyhow!("Touch waiting cancelled"));
            }

            // Process events in a short timeout window to allow cancellation checking
            let events_result = timeout(Duration::from_millis(100), async {
                let mut events_to_process = Vec::new();
                if let Some(device) = device {
                    // Note: fetch_events() is still blocking, but we timeout quickly
                    // In a full async implementation, we'd use async evdev or epoll
                    for event in device.fetch_events()? {
                        events_to_process.push(event);
                    }
                }
                Ok::<Vec<InputEvent>, anyhow::Error>(events_to_process)
            })
            .await;

            match events_result {
                Ok(Ok(events_to_process)) => {
                    // Process the events after getting them
                    for event in events_to_process {
                        if event.code() == ABS_MT_POSITION_X {
                            position_x = event.value();
                        }
                        if event.code() == ABS_MT_POSITION_Y {
                            position_y = event.value();
                        }
                        if event.code() == ABS_MT_TRACKING_ID && event.value() == -1 {
                            let (x, y) = Self::input_to_virtual_static((position_x, position_y), device_model);
                            debug!("Touch release detected at ({}, {}) normalized ({}, {})", position_x, position_y, x, y);
                            if Self::is_in_trigger_zone_static(x, y, trigger_corner) {
                                debug!("Touch release in target zone!");
                                return Ok(());
                            }
                        }
                    }
                }
                Ok(Err(e)) => {
                    // Error reading events, continue loop
                    debug!("Error reading touch events: {}", e);
                }
                Err(_) => {
                    // Timeout - this is expected, allows us to check cancellation
                    // No events within timeout window, continue checking
                }
            }

            // Small yield to prevent busy waiting
            sleep(Duration::from_millis(10)).await;
        }
    }

    pub async fn touch_start(&mut self, xy: (i32, i32)) -> Result<()> {
        match &mut self.mode {
            TouchMode::Simulated { .. } => {
                debug!("Simulated touch_start at ({}, {})", xy.0, xy.1);
                Ok(())
            }
            TouchMode::Real { device, device_model } => {
                let (x, y) = Self::virtual_to_input_static(xy, device_model);
                if let Some(device) = device {
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
            TouchMode::Real { device, .. } => {
                if let Some(device) = device {
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
            TouchMode::Real { device, device_model } => {
                let (x, y) = Self::virtual_to_input_static(xy, device_model);
                if let Some(device) = device {
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

    fn is_in_trigger_zone(&self, x: i32, y: i32) -> bool {
        Self::is_in_trigger_zone_static(x, y, self.trigger_corner)
    }

    fn is_in_trigger_zone_static(x: i32, y: i32, trigger_corner: TriggerCorner) -> bool {
        const CORNER_SIZE: i32 = 68; // Size of the trigger zone (68x68 pixels)

        match trigger_corner {
            TriggerCorner::UpperRight => x > VIRTUAL_WIDTH as i32 - CORNER_SIZE && y < CORNER_SIZE,
            TriggerCorner::UpperLeft => x < CORNER_SIZE && y < CORNER_SIZE,
            TriggerCorner::LowerRight => x > VIRTUAL_WIDTH as i32 - CORNER_SIZE && y > VIRTUAL_HEIGHT as i32 - CORNER_SIZE,
            TriggerCorner::LowerLeft => x < CORNER_SIZE && y > VIRTUAL_HEIGHT as i32 - CORNER_SIZE,
        }
    }

    fn screen_width(&self) -> u32 {
        let device_model = match &self.mode {
            TouchMode::Real { device_model, .. } => device_model,
            TouchMode::Simulated { .. } => &DeviceModel::Unknown, // Default for simulation
        };
        match device_model {
            DeviceModel::Remarkable2 => 1404,
            DeviceModel::RemarkablePaperPro => 2065,
            DeviceModel::Unknown => 1404, // Default to RM2
        }
    }

    fn screen_height(&self) -> u32 {
        let device_model = match &self.mode {
            TouchMode::Real { device_model, .. } => device_model,
            TouchMode::Simulated { .. } => &DeviceModel::Unknown, // Default for simulation
        };
        match device_model {
            DeviceModel::Remarkable2 => 1872,
            DeviceModel::RemarkablePaperPro => 2833,
            DeviceModel::Unknown => 1872, // Default to RM2
        }
    }

    fn virtual_to_input(&self, (x, y): (i32, i32)) -> (i32, i32) {
        let device_model = match &self.mode {
            TouchMode::Real { device_model, .. } => device_model,
            TouchMode::Simulated { .. } => &DeviceModel::Unknown, // Default for simulation
        };
        Self::virtual_to_input_static((x, y), device_model)
    }

    fn virtual_to_input_static((x, y): (i32, i32), device_model: &DeviceModel) -> (i32, i32) {
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

    fn input_to_virtual(&self, (x, y): (i32, i32)) -> (i32, i32) {
        let device_model = match &self.mode {
            TouchMode::Real { device_model, .. } => device_model,
            TouchMode::Simulated { .. } => &DeviceModel::Unknown, // Default for simulation
        };
        Self::input_to_virtual_static((x, y), device_model)
    }

    fn input_to_virtual_static((x, y): (i32, i32), device_model: &DeviceModel) -> (i32, i32) {
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
