//! Maps held keys into the same flight axes as other controls.

use bevy::prelude::*;

/// Local movement and turning, with optional boost.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct FlightAxes {
    /// See [`crate::fly_camera::FlyInput::move_local`].
    pub move_local: Vec3,
    /// See [`crate::fly_camera::FlyInput::turn`].
    pub turn: f32,
    pub boost: bool,
}

/// Whether `navigator.platform` names a Mac (`"MacIntel"` in every current Mac browser).
pub fn is_mac_platform(platform: &str) -> bool {
    platform.starts_with("Mac")
}

/// Whether the game runs on a Mac: natively by the target, on the web by `navigator.platform`
/// (read once). A Mac browser has the native app's clash (Mission Control is the system's, so a
/// page cannot take the keys back), and elsewhere a browser's `Alt` with the arrows is Back and
/// Forward, so the web follows the machine, not the build.
pub fn platform_is_mac() -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        static MAC: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *MAC.get_or_init(|| {
            web_sys::window()
                .and_then(|w| w.navigator().platform().ok())
                .is_some_and(|p| is_mac_platform(&p))
        })
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        cfg!(target_os = "macos")
    }
}

/// The modifier keys that descend (with `Q`, everywhere): `Option` on a Mac, where `Ctrl+↑`
/// opens Mission Control and `Ctrl+←` / `→` switch Spaces; `Ctrl` elsewhere. Not `Cmd`: `⌘Q`
/// quits, and `Q` is the other descend key.
pub fn descend_modifiers(mac: bool) -> [KeyCode; 2] {
    if mac {
        [KeyCode::AltLeft, KeyCode::AltRight]
    } else {
        [KeyCode::ControlLeft, KeyCode::ControlRight]
    }
}

/// The held keys as flight input:
/// - `W` / `↑` forward, `S` / `↓` back, `A` / `D` sideways;
/// - `Space` / `E` up, `Q` or the descend modifier ([`descend_modifiers`]: `Option` on a Mac,
///   `Ctrl` elsewhere) down;
/// - `←` / `→` turn, or fly sideways while `Shift` is held;
/// - `Shift` with forward boosts. It boosts nothing else, since with the arrows it means
///   "sideways".
pub fn keys_to_input(keys: &ButtonInput<KeyCode>) -> FlightAxes {
    let held = |codes: &[KeyCode]| keys.any_pressed(codes.iter().copied());
    let axis = |plus: bool, minus: bool| f32::from(u8::from(plus)) - f32::from(u8::from(minus));
    let shift = held(&[KeyCode::ShiftLeft, KeyCode::ShiftRight]);
    let left = held(&[KeyCode::ArrowLeft]);
    let right = held(&[KeyCode::ArrowRight]);
    let (strafe_left, strafe_right) = if shift { (left, right) } else { (false, false) };
    let move_local = Vec3::new(
        axis(
            held(&[KeyCode::KeyD]) || strafe_right,
            held(&[KeyCode::KeyA]) || strafe_left,
        ),
        axis(
            held(&[KeyCode::Space, KeyCode::KeyE]),
            held(&descend_modifiers(platform_is_mac())) || held(&[KeyCode::KeyQ]),
        ),
        axis(
            held(&[KeyCode::KeyS, KeyCode::ArrowDown]),
            held(&[KeyCode::KeyW, KeyCode::ArrowUp]),
        ),
    );
    FlightAxes {
        move_local,
        turn: if shift { 0.0 } else { axis(right, left) },
        boost: shift && move_local.z < 0.0,
    }
}
