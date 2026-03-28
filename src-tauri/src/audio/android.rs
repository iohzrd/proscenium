//! Android-specific audio routing via AudioManager JNI calls.
//!
//! On Android, cpal cannot switch between bluetooth/speaker/earpiece --
//! that's an OS-level concern handled by AudioManager. This module wraps
//! the necessary JNI calls.

#[cfg(target_os = "android")]
use jni::JNIEnv;
#[cfg(target_os = "android")]
use jni::objects::{JObject, JValueGen};

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

/// Helper: get AudioManager from the Android context.
#[cfg(target_os = "android")]
fn get_audio_manager<'a>(env: &mut JNIEnv<'a>) -> Result<JObject<'a>, String> {
    let ctx = ndk_context::android_context();
    let activity = unsafe { JObject::from_raw(ctx.context().cast()) };
    let audio_service = env
        .new_string("audio")
        .map_err(|e| format!("JNI string error: {e}"))?;
    env.call_method(
        &activity,
        "getSystemService",
        "(Ljava/lang/String;)Ljava/lang/Object;",
        &[JValueGen::Object(&audio_service.into())],
    )
    .map_err(|e| format!("getSystemService failed: {e}"))?
    .l()
    .map_err(|e| format!("getSystemService return type error: {e}"))
}

/// List available communication audio devices (API 31+).
#[cfg(target_os = "android")]
pub fn list_communication_devices() -> Result<Vec<CommAudioDevice>, String> {
    let ctx = ndk_context::android_context();
    let vm = unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) }
        .map_err(|e| format!("failed to get JavaVM: {e}"))?;
    let mut env = vm
        .attach_current_thread()
        .map_err(|e| format!("failed to attach JNI thread: {e}"))?;

    let audio_manager = get_audio_manager(&mut env)?;

    let devices_list = match env.call_method(
        &audio_manager,
        "getAvailableCommunicationDevices",
        "()Ljava/util/List;",
        &[],
    ) {
        Ok(result) => result
            .l()
            .map_err(|e| format!("getAvailableCommunicationDevices type error: {e}"))?,
        Err(_) => return Ok(Vec::new()),
    };

    let size = env
        .call_method(&devices_list, "size", "()I", &[])
        .map_err(|e| format!("List.size failed: {e}"))?
        .i()
        .map_err(|e| format!("List.size type error: {e}"))?;

    let mut devices = Vec::new();
    for i in 0..size {
        let device = env
            .call_method(
                &devices_list,
                "get",
                "(I)Ljava/lang/Object;",
                &[JValueGen::Int(i)],
            )
            .map_err(|e| format!("List.get failed: {e}"))?
            .l()
            .map_err(|e| format!("List.get type error: {e}"))?;

        let device_type = env
            .call_method(&device, "getType", "()I", &[])
            .map_err(|e| format!("getType failed: {e}"))?
            .i()
            .map_err(|e| format!("getType type error: {e}"))?;

        let id = env
            .call_method(&device, "getId", "()I", &[])
            .map_err(|e| format!("getId failed: {e}"))?
            .i()
            .map_err(|e| format!("getId type error: {e}"))?;

        let product_name_obj = env
            .call_method(&device, "getProductName", "()Ljava/lang/CharSequence;", &[])
            .map_err(|e| format!("getProductName failed: {e}"))?
            .l()
            .map_err(|e| format!("getProductName type error: {e}"))?;

        let product_name: String = if product_name_obj.is_null() {
            String::new()
        } else {
            // CharSequence.toString() -> String
            let name_jstr = env
                .call_method(&product_name_obj, "toString", "()Ljava/lang/String;", &[])
                .and_then(|v| v.l());
            match name_jstr {
                Ok(obj) => {
                    let jstr = env.get_string((&obj).into());
                    match jstr {
                        Ok(s) => String::from(s),
                        Err(_) => String::new(),
                    }
                }
                Err(_) => String::new(),
            }
        };

        let label = type_label(device_type, &product_name);

        devices.push(CommAudioDevice {
            id,
            name: product_name,
            device_type,
            label,
        });
    }

    Ok(devices)
}

/// Set a specific communication device by ID (API 31+).
#[cfg(target_os = "android")]
pub fn set_communication_device_by_id(device_id: i32) -> Result<(), String> {
    let ctx = ndk_context::android_context();
    let vm = unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) }
        .map_err(|e| format!("failed to get JavaVM: {e}"))?;
    let mut env = vm
        .attach_current_thread()
        .map_err(|e| format!("failed to attach JNI thread: {e}"))?;

    let audio_manager = get_audio_manager(&mut env)?;

    // Set MODE_IN_COMMUNICATION
    env.call_method(&audio_manager, "setMode", "(I)V", &[JValueGen::Int(3)])
        .map_err(|e| format!("setMode failed: {e}"))?;

    // For speaker (type 2), use setSpeakerphoneOn instead of setCommunicationDevice
    // because setCommunicationDevice with the built-in speaker may not work on all devices.
    // First, find the device to check its type.
    let devices_list = env
        .call_method(
            &audio_manager,
            "getAvailableCommunicationDevices",
            "()Ljava/util/List;",
            &[],
        )
        .map_err(|e| format!("getAvailableCommunicationDevices failed: {e}"))?
        .l()
        .map_err(|e| format!("getAvailableCommunicationDevices type error: {e}"))?;

    let size = env
        .call_method(&devices_list, "size", "()I", &[])
        .map_err(|e| format!("List.size failed: {e}"))?
        .i()
        .map_err(|e| format!("List.size type error: {e}"))?;

    for i in 0..size {
        let device = env
            .call_method(
                &devices_list,
                "get",
                "(I)Ljava/lang/Object;",
                &[JValueGen::Int(i)],
            )
            .map_err(|e| format!("List.get failed: {e}"))?
            .l()
            .map_err(|e| format!("List.get type error: {e}"))?;

        let id = env
            .call_method(&device, "getId", "()I", &[])
            .map_err(|e| format!("getId failed: {e}"))?
            .i()
            .map_err(|e| format!("getId type error: {e}"))?;

        if id == device_id {
            let device_type = env
                .call_method(&device, "getType", "()I", &[])
                .map_err(|e| format!("getType failed: {e}"))?
                .i()
                .map_err(|e| format!("getType type error: {e}"))?;

            // TYPE_BUILTIN_SPEAKER = 2: use setSpeakerphoneOn
            if device_type == 2 {
                let _ = env.call_method(&audio_manager, "clearCommunicationDevice", "()V", &[]);
                env.call_method(
                    &audio_manager,
                    "setSpeakerphoneOn",
                    "(Z)V",
                    &[JValueGen::Bool(1)],
                )
                .map_err(|e| format!("setSpeakerphoneOn failed: {e}"))?;
                log::info!("[audio-android] set route to speaker via setSpeakerphoneOn");
                return Ok(());
            }

            // For earpiece / bluetooth / anything else: use setCommunicationDevice
            env.call_method(
                &audio_manager,
                "setSpeakerphoneOn",
                "(Z)V",
                &[JValueGen::Bool(0)],
            )
            .map_err(|e| format!("setSpeakerphoneOn(false) failed: {e}"))?;

            let result = env
                .call_method(
                    &audio_manager,
                    "setCommunicationDevice",
                    "(Landroid/media/AudioDeviceInfo;)Z",
                    &[JValueGen::Object(&device)],
                )
                .map_err(|e| format!("setCommunicationDevice failed: {e}"))?
                .z()
                .map_err(|e| format!("setCommunicationDevice type error: {e}"))?;

            if result {
                log::info!(
                    "[audio-android] setCommunicationDevice to id={device_id} type={device_type}"
                );
                return Ok(());
            } else {
                return Err(format!(
                    "setCommunicationDevice returned false for id={device_id}"
                ));
            }
        }
    }

    Err(format!("device id {device_id} not found"))
}

/// Restore default audio routing (call this when a call ends).
#[cfg(target_os = "android")]
pub fn restore_default_routing() {
    let ctx = ndk_context::android_context();
    let Ok(vm) = (unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) }) else {
        return;
    };
    let Ok(mut env) = vm.attach_current_thread() else {
        return;
    };
    let Ok(am) = get_audio_manager(&mut env) else {
        return;
    };

    let _ = env.call_method(&am, "setSpeakerphoneOn", "(Z)V", &[JValueGen::Bool(0)]);
    let _ = env.call_method(&am, "setBluetoothScoOn", "(Z)V", &[JValueGen::Bool(0)]);
    let _ = env.call_method(&am, "stopBluetoothSco", "()V", &[]);
    let _ = env.call_method(&am, "clearCommunicationDevice", "()V", &[]);
    // MODE_NORMAL = 0
    let _ = env.call_method(&am, "setMode", "(I)V", &[JValueGen::Int(0)]);
    log::info!("[audio-android] restored default routing");
}

/// Set MODE_IN_COMMUNICATION before opening audio streams.
#[cfg(target_os = "android")]
pub fn enter_communication_mode() {
    let ctx = ndk_context::android_context();
    let Ok(vm) = (unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) }) else {
        return;
    };
    let Ok(mut env) = vm.attach_current_thread() else {
        return;
    };
    let Ok(am) = get_audio_manager(&mut env) else {
        return;
    };
    let _ = env.call_method(&am, "setMode", "(I)V", &[JValueGen::Int(3)]);
    log::info!("[audio-android] set MODE_IN_COMMUNICATION");
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
