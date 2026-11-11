use serde::{Deserialize, Serialize};

use crate::encoding::Codec;

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum InputEvent {
    Pointer {
        x: f64,
        y: f64,
    },
    Button {
        button: String,
        down: bool,
        x: f64,
        y: f64,
    },
    Scroll {
        dx: i32,
        dy: i32,
    },
    Key {
        code: String,
        key: String,
        down: bool,
    },
    Text {
        text: String,
    },
    Release,
}

impl InputEvent {
    pub fn validate(&self) -> Result<(), &'static str> {
        let position = |x: f64, y: f64| {
            x.is_finite() && y.is_finite() && (0.0..=1.0).contains(&x) && (0.0..=1.0).contains(&y)
        };
        let valid = match self {
            Self::Pointer { x, y } => position(*x, *y),
            Self::Button { button, x, y, .. } => {
                position(*x, *y) && ["left", "middle", "right"].contains(&button.as_str())
            }
            Self::Scroll { dx, dy } => (-20..=20).contains(dx) && (-20..=20).contains(dy),
            Self::Key { code, key, .. } => !code.is_empty() && code.len() <= 40 && key.len() <= 40,
            Self::Text { text } => {
                !text.is_empty() && text.chars().count() <= 2000 && !text.contains('\0')
            }
            Self::Release => true,
        };
        if valid {
            Ok(())
        } else {
            Err("입력 값이 허용 범위를 벗어났습니다.")
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StreamSettings {
    pub monitor: u32,
    pub fps: u32,
    pub quality: u8,
    pub max_width: u32,
    #[serde(default = "default_codec")]
    pub codec: Codec,
}

fn default_codec() -> Codec {
    Codec::Jpeg
}

impl Default for StreamSettings {
    fn default() -> Self {
        Self {
            monitor: 1,
            fps: 60,
            quality: 45,
            max_width: 1280,
            codec: Codec::Jpeg,
        }
    }
}

impl StreamSettings {
    pub fn validate(&self, monitors: &[u32]) -> Result<(), &'static str> {
        if !monitors.contains(&self.monitor)
            || !(1..=60).contains(&self.fps)
            || !(30..=90).contains(&self.quality)
            || !(640..=2560).contains(&self.max_width)
        {
            return Err("화면 또는 전송 설정이 허용 범위를 벗어났습니다.");
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ControlMessage {
    Ack,
    Ping {
        sent: f64,
    },
    Settings {
        monitor: u32,
        fps: u32,
        quality: u8,
        max_width: u32,
        #[serde(default = "default_codec")]
        codec: Codec,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unbounded_input() {
        assert!(
            InputEvent::Pointer {
                x: f64::NAN,
                y: 0.5
            }
            .validate()
            .is_err()
        );
        assert!(InputEvent::Pointer { x: 1.01, y: 0.5 }.validate().is_err());
        assert!(
            InputEvent::Scroll {
                dx: i32::MIN,
                dy: 0
            }
            .validate()
            .is_err()
        );
        assert!(
            InputEvent::Text {
                text: "가".repeat(2000)
            }
            .validate()
            .is_ok()
        );
        assert!(
            InputEvent::Text {
                text: "가".repeat(2001)
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn validates_monitor_and_bandwidth_settings() {
        let mut settings = StreamSettings::default();
        assert!(settings.validate(&[1]).is_ok());
        settings.monitor = 999;
        assert!(settings.validate(&[1]).is_err());
        settings.monitor = 1;
        settings.fps = 61;
        assert!(settings.validate(&[1]).is_err());
    }
}
