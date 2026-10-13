//! Android-specific audio routing via AudioManager JNI calls.
//!
//! On Android, cpal cannot switch between bluetooth/speaker/earpiece --
//! that's an OS-level concern handled by AudioManager. This module wraps
//! the necessary JNI calls.

#[cfg(target_os = "android")]
use jni::errors::Error as JniError;
#[cfg(target_os = "android")]
use jni::objects::{JObject, JString};
#[cfg(target_os = "android")]
use jni::signature::MethodSignature;
#[cfg(target_os = "android")]
use jni::strings::JNIStr;
#[cfg(target_os = "android")]
use jni::{Env, JValue, JValueOwned, JavaVM, jni_sig, jni_str};

use serde::Serialize;

/// A communication audio endpoint returned by `getAvailableCommunicationDevices`.
#[derive(Debug, Clone, Serialize)]
pub struct CommAudioDevice {
    /// Android AudioDeviceInfo ID.
    pub id: i32,
    /// Human-readable product name (e.g. "WI-C100", or empty for built-in).
    pub name: String,
    /// Device type constant from AudioDeviceInfo.
    pub device_type: i32,
    /// Friendly label derived from the device type.
    pub label: String,
}

/// Map an AudioDeviceInfo type constant to a human label.
#[cfg(target_os = "android")]
fn type_label(device_type: i32, product_name: &str) -> String {
    match device_type {
        // TYPE_BUILTIN_EARPIECE = 1
        1 => "Phone earpiece".to_string(),
        // TYPE_BUILTIN_SPEAKER = 2
        2 => "Speaker".to_string(),
        // TYPE_WIRED_HEADSET = 3
        3 => {
            if product_name.is_empty() {
                "Wired headset".to_string()
            } else {
                product_name.to_string()
            }
        }
        // TYPE_WIRED_HEADPHONES = 4
        4 => {
            if product_name.is_empty() {
                "Wired headphones".to_string()
            } else {
                product_name.to_string()
            }
        }
        // TYPE_BLUETOOTH_SCO = 7
        7 => {
            if product_name.is_empty() {
                "Bluetooth".to_string()
            } else {
                product_name.to_string()
            }
        }
        // TYPE_USB_HEADSET = 22
        22 => {
            if product_name.is_empty() {
                "USB headset".to_string()
            } else {
                product_name.to_string()
            }
        }
        // TYPE_BLE_HEADSET = 26
        26 => {
            if product_name.is_empty() {
                "Bluetooth LE".to_string()
            } else {
                product_name.to_string()
            }
        }
        // TYPE_BLE_SPEAKER = 27
        27 => {
            if product_name.is_empty() {
                "Bluetooth LE speaker".to_string()
            } else {
                product_name.to_string()
            }
        }
        _ => {
            if product_name.is_empty() {
                format!("Audio device (type {device_type})")
            } else {
                product_name.to_string()
            }
        }
    }
}

/// First API level with `AudioManager.getAvailableCommunicationDevices`,
/// `setCommunicationDevice` and `clearCommunicationDevice` (Android 12).
#[cfg(target_os = "android")]
const API_LEVEL_COMMUNICATION_DEVICE: i32 = 31;
/// `AudioManager.MODE_NORMAL`.
#[cfg(target_os = "android")]
const MODE_NORMAL: i32 = 0;
/// `AudioManager.MODE_IN_COMMUNICATION`.
#[cfg(target_os = "android")]
const MODE_IN_COMMUNICATION: i32 = 3;
/// `AudioDeviceInfo.TYPE_BUILTIN_SPEAKER`.
#[cfg(target_os = "android")]
const TYPE_BUILTIN_SPEAKER: i32 = 2;

/// Error of an audio routing operation.
#[cfg(target_os = "android")]
enum RouteError {
    Jni(JniError),
    Other(String),
}

#[cfg(target_os = "android")]
impl From<JniError> for RouteError {
    fn from(e: JniError) -> Self {
        Self::Jni(e)
    }
}

#[cfg(target_os = "android")]
impl std::fmt::Display for RouteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Jni(e) => write!(f, "JNI error: {e}"),
            Self::Other(msg) => f.write_str(msg),
        }
    }
}

/// Call a Java method. If the call fails, clear the pending Java exception so that
/// later JNI calls on this thread are valid.
#[cfg(target_os = "android")]
fn call<'local, 'sig, 'sig_args>(
    env: &mut Env<'local>,
    obj: &JObject<'_>,
    name: &JNIStr,
    sig: &MethodSignature<'sig, 'sig_args>,
    args: &[JValue],
) -> Result<JValueOwned<'local>, JniError> {
    let result = env.call_method(obj, name, sig, args);
    if result.is_err() && env.exception_check() {
        env.exception_clear();
    }
    result
}

/// Attach the current thread to the JVM, fetch the `AudioManager` system service and
/// run `f` with it.
#[cfg(target_os = "android")]
fn with_audio_manager<T>(
    f: impl FnOnce(&mut Env<'_>, &JObject<'_>) -> Result<T, RouteError>,
) -> Result<T, String> {
    let ctx = ndk_context::android_context();
    let vm_ptr = ctx.vm();
    let context_ptr = ctx.context();
    if vm_ptr.is_null() || context_ptr.is_null() {
        return Err("Android context is not initialized".to_string());
    }
    // SAFETY: `vm_ptr` is the non-null JavaVM pointer the Android runtime registered
    // with ndk_context; it stays valid for the lifetime of the process.
    let vm = unsafe { JavaVM::from_raw(vm_ptr.cast()) };
    vm.attach_current_thread(|env: &mut Env<'_>| -> Result<T, RouteError> {
        // SAFETY: `context_ptr` is a global reference to the application Context held
        // by ndk_context for the lifetime of the process. `JObject` does not delete the
        // reference when dropped.
        let context = unsafe { JObject::from_raw(env, context_ptr.cast()) };
        let service_name = env.new_string("audio")?;
        let audio_manager = call(
            env,
            &context,
            jni_str!("getSystemService"),
            &jni_sig!("(Ljava/lang/String;)Ljava/lang/Object;"),
            &[(&service_name).into()],
        )?
        .l()?;
        if audio_manager.is_null() {
            return Err(RouteError::Other("AudioManager is not available".into()));
        }
        f(env, &audio_manager)
    })
    .map_err(|e| e.to_string())
}

/// Read `android.os.Build.VERSION.SDK_INT`.
#[cfg(target_os = "android")]
fn sdk_level(env: &mut Env<'_>) -> Result<i32, JniError> {
    env.get_static_field(
        jni_str!("android/os/Build$VERSION"),
        jni_str!("SDK_INT"),
        jni_sig!("I"),
    )?
    .i()
}

/// Call `AudioManager.getAvailableCommunicationDevices()` (API 31+) and return the
/// elements of the list.
#[cfg(target_os = "android")]
fn communication_devices<'local>(
    env: &mut Env<'local>,
    audio_manager: &JObject<'_>,
) -> Result<Vec<JObject<'local>>, JniError> {
    let list = call(
        env,
        audio_manager,
        jni_str!("getAvailableCommunicationDevices"),
        &jni_sig!("()Ljava/util/List;"),
        &[],
    )?
    .l()?;
    let size = call(env, &list, jni_str!("size"), &jni_sig!("()I"), &[])?.i()?;
    let mut devices = Vec::with_capacity(usize::try_from(size).unwrap_or(0));
    for i in 0..size {
        let device = call(
            env,
            &list,
            jni_str!("get"),
            &jni_sig!("(I)Ljava/lang/Object;"),
            &[JValue::Int(i)],
        )?
        .l()?;
        devices.push(device);
    }
    Ok(devices)
}

/// Read the product name of an `AudioDeviceInfo`. Empty if it has none.
#[cfg(target_os = "android")]
fn product_name(env: &mut Env<'_>, device: &JObject<'_>) -> String {
    let read = |env: &mut Env<'_>| -> Result<String, JniError> {
        let chars = call(
            env,
            device,
            jni_str!("getProductName"),
            &jni_sig!("()Ljava/lang/CharSequence;"),
            &[],
        )?
        .l()?;
        if chars.is_null() {
            return Ok(String::new());
        }
        let text = call(
            env,
            &chars,
            jni_str!("toString"),
            &jni_sig!("()Ljava/lang/String;"),
            &[],
        )?
        .l()?;
        if text.is_null() {
            return Ok(String::new());
        }
        let text = env.cast_local::<JString>(text)?;
        text.try_to_string(env)
    };
    read(env).unwrap_or_default()
}

/// List available communication audio devices. Empty below API 31.
#[cfg(target_os = "android")]
pub fn list_communication_devices() -> Result<Vec<CommAudioDevice>, String> {
    with_audio_manager(|env, audio_manager| {
        if sdk_level(env)? < API_LEVEL_COMMUNICATION_DEVICE {
            return Ok(Vec::new());
        }
        let mut devices = Vec::new();
        for device in communication_devices(env, audio_manager)? {
            let device_type =
                call(env, &device, jni_str!("getType"), &jni_sig!("()I"), &[])?.i()?;
            let id = call(env, &device, jni_str!("getId"), &jni_sig!("()I"), &[])?.i()?;
            let name = product_name(env, &device);
            let label = type_label(device_type, &name);
            devices.push(CommAudioDevice {
                id,
                name,
                device_type,
                label,
            });
        }
        Ok(devices)
    })
}

/// Route call audio to the communication device with the given id (API 31+).
/// `MODE_IN_COMMUNICATION` is set only after the route change succeeded.
#[cfg(target_os = "android")]
pub fn set_communication_device_by_id(device_id: i32) -> Result<(), String> {
    with_audio_manager(|env, audio_manager| {
        if sdk_level(env)? < API_LEVEL_COMMUNICATION_DEVICE {
            return Err(RouteError::Other(
                "selecting a communication device requires Android 12".into(),
            ));
        }

        let mut selected = None;
        for device in communication_devices(env, audio_manager)? {
            let id = call(env, &device, jni_str!("getId"), &jni_sig!("()I"), &[])?.i()?;
            if id == device_id {
                selected = Some(device);
                break;
            }
        }
        let Some(device) = selected else {
            return Err(RouteError::Other(format!(
                "device id {device_id} not found"
            )));
        };
        let device_type = call(env, &device, jni_str!("getType"), &jni_sig!("()I"), &[])?.i()?;

        // The built-in speaker is selected with setSpeakerphoneOn:
        // setCommunicationDevice does not accept it on all devices.
        if device_type == TYPE_BUILTIN_SPEAKER {
            call(
                env,
                audio_manager,
                jni_str!("clearCommunicationDevice"),
                &jni_sig!("()V"),
                &[],
            )?;
            call(
                env,
                audio_manager,
                jni_str!("setSpeakerphoneOn"),
                &jni_sig!("(Z)V"),
                &[JValue::Bool(true)],
            )?;
        } else {
            call(
                env,
                audio_manager,
                jni_str!("setSpeakerphoneOn"),
                &jni_sig!("(Z)V"),
                &[JValue::Bool(false)],
            )?;
            let accepted = call(
                env,
                audio_manager,
                jni_str!("setCommunicationDevice"),
                &jni_sig!("(Landroid/media/AudioDeviceInfo;)Z"),
                &[(&device).into()],
            )?
            .z()?;
            if !accepted {
                return Err(RouteError::Other(format!(
                    "setCommunicationDevice returned false for id={device_id}"
                )));
            }
        }

        call(
            env,
            audio_manager,
            jni_str!("setMode"),
            &jni_sig!("(I)V"),
            &[JValue::Int(MODE_IN_COMMUNICATION)],
        )?;
        log::info!("[audio-android] routed call audio to id={device_id} type={device_type}");
        Ok(())
    })
}

/// Restore default audio routing (call this when a call ends).
#[cfg(target_os = "android")]
pub fn restore_default_routing() {
    let result = with_audio_manager(|env, audio_manager| {
        // Each step is independent; a failed step does not skip the following ones.
        let _ = call(
            env,
            audio_manager,
            jni_str!("setSpeakerphoneOn"),
            &jni_sig!("(Z)V"),
            &[JValue::Bool(false)],
        );
        let _ = call(
            env,
            audio_manager,
            jni_str!("setBluetoothScoOn"),
            &jni_sig!("(Z)V"),
            &[JValue::Bool(false)],
        );
        let _ = call(
            env,
            audio_manager,
            jni_str!("stopBluetoothSco"),
            &jni_sig!("()V"),
            &[],
        );
        if sdk_level(env)? >= API_LEVEL_COMMUNICATION_DEVICE {
            let _ = call(
                env,
                audio_manager,
                jni_str!("clearCommunicationDevice"),
                &jni_sig!("()V"),
                &[],
            );
        }
        call(
            env,
            audio_manager,
            jni_str!("setMode"),
            &jni_sig!("(I)V"),
            &[JValue::Int(MODE_NORMAL)],
        )?;
        Ok(())
    });
    match result {
        Ok(()) => log::info!("[audio-android] restored default routing"),
        Err(e) => log::warn!("[audio-android] failed to restore default routing: {e}"),
    }
}

/// Set MODE_IN_COMMUNICATION before opening audio streams.
#[cfg(target_os = "android")]
pub fn enter_communication_mode() {
    let result = with_audio_manager(|env, audio_manager| {
        call(
            env,
            audio_manager,
            jni_str!("setMode"),
            &jni_sig!("(I)V"),
            &[JValue::Int(MODE_IN_COMMUNICATION)],
        )?;
        Ok(())
    });
    match result {
        Ok(()) => log::info!("[audio-android] set MODE_IN_COMMUNICATION"),
        Err(e) => log::warn!("[audio-android] failed to set MODE_IN_COMMUNICATION: {e}"),
    }
}

// Stubs for non-Android platforms
#[cfg(not(target_os = "android"))]
pub fn list_communication_devices() -> Result<Vec<CommAudioDevice>, String> {
    Ok(Vec::new())
}

#[cfg(not(target_os = "android"))]
pub fn set_communication_device_by_id(_device_id: i32) -> Result<(), String> {
    Ok(())
}

#[cfg(not(target_os = "android"))]
pub fn restore_default_routing() {}

#[cfg(not(target_os = "android"))]
pub fn enter_communication_mode() {}
