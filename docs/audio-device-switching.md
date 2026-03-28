# Audio Device Selection and Mid-Call Switching

## Overview

Audio device selection allows users to choose which microphone and speaker to use for calls. On desktop (Linux/macOS/Windows), this is done by enumerating cpal devices and rebuilding streams on the selected device. On Android, audio routing is an OS-level concern handled by AudioManager via JNI.

## Desktop (cpal)

### Device Enumeration

cpal's `Host::input_devices()` and `Host::output_devices()` return iterators over available audio devices. Each device has a description/name. On Android, cpal returns many duplicates for the same physical device (different sample rates, formats, endpoints) so results must be deduplicated by name.

### Device Selection

cpal streams are permanently bound to the device they were built on. There is no `Stream::set_device()` API. To switch devices, you must tear down the old stream and build a new one on the target device.

### Mid-Call Hot-Swap

The key to seamless switching is the decoupling layer between the audio hardware and the network/codec pipeline:

- **Capture (input):** The cpal input callback sends samples into a tokio mpsc channel. The encoder reads from the channel. When switching devices, clone the same Sender into the new stream's callback. The encoder never knows the stream changed -- it sees at most a brief pause in samples, which Opus handles gracefully.

- **Playback (output):** The decode thread pushes samples into a lock-free ring buffer (ringbuf). The cpal output callback pops from it. When switching devices, create a new ring buffer, swap the producer under a mutex so the decode thread starts writing to the new buffer, then build a new output stream with the new buffer's consumer. The old stream and old buffer are dropped.

The glitch during a switch is ~10-20ms of silence, which is inaudible in practice.

### Gotcha: Send Safety

cpal::Stream does not implement Send because some niche backends (ASIO) have thread-affinity requirements. For the backends we target (ALSA, AAudio, CoreAudio, WASAPI), the handles are safe to move between threads. An explicit `unsafe impl Send` is required.

## Android (AudioManager via JNI)

### Why cpal Alone Doesn't Work

On Android, cpal uses AAudio/Oboe under the hood. While cpal can enumerate devices, selecting a specific device by name does not change the OS-level audio routing. The phone's built-in mic remains active even if you "select" the bluetooth device in cpal. Audio routing on Android is managed by the system's AudioManager.

### MODE_IN_COMMUNICATION

Before opening any audio streams for a call, the app must set `AudioManager.setMode(MODE_IN_COMMUNICATION)` (mode value 3). This tells Android the app is in a VoIP call, which:
- Enables communication audio routing (earpiece, bluetooth SCO)
- May cause cpal to pick the bluetooth mic as the default input device automatically
- Is required before `setCommunicationDevice` will work

Must be restored to `MODE_NORMAL` (0) when the call ends.

### setCommunicationDevice (API 31+)

`AudioManager.setCommunicationDevice(AudioDeviceInfo)` is the modern way to route audio on Android 12+. It takes an AudioDeviceInfo object obtained from `getAvailableCommunicationDevices()`.

Relevant device type constants from AudioDeviceInfo:
- TYPE_BLUETOOTH_SCO = 7
- TYPE_BLE_HEADSET = 26

`clearCommunicationDevice()` reverts to default routing.

### Fallback: Bluetooth SCO (pre-API 31)

For older Android versions, bluetooth mic routing requires:
1. `AudioManager.startBluetoothSco()` to initiate the SCO audio link
2. `AudioManager.setBluetoothScoOn(true)` to route audio through it
3. Reverse both when switching away from bluetooth

### Speaker routing

`AudioManager.setSpeakerphoneOn(true/false)` toggles speakerphone. Must disable speaker before enabling bluetooth, and vice versa.

### Critical Discovery: setCommunicationDevice Kills Existing Streams

When `setCommunicationDevice` is called, Android sends an `onAudioDeviceUpdate` callback to all active AAudio streams, changing their device IDs (e.g., `21 => 1054`). This triggers a `DISCONNECT` request inside the AAudio legacy layer. The streams die with error "The requested device is no longer available."

cpal has no auto-reconnect mechanism. The stream error callback fires, but the stream is dead.

**The solution:** After calling `setCommunicationDevice`, wait ~1 second for Android to finish disconnecting the old streams, then rebuild both capture and playback streams. The new streams open on the "Default Device" which now points to the bluetooth device (because `setCommunicationDevice` changed the routing). This results in ~1 second of silence during the switch.

### Android Permissions Required

- `RECORD_AUDIO` (runtime permission, must be requested)
- `MODIFY_AUDIO_SETTINGS` (normal permission, auto-granted)
- `BLUETOOTH_CONNECT` (runtime permission on API 31+)

### JNI Access Pattern

The Android context (JavaVM pointer and Activity jobject) is obtained via `ndk_context::android_context()`. The jni crate provides `JavaVM::from_raw()` and `attach_current_thread()` to get a JNIEnv for calling Java methods. AudioManager is obtained via `Activity.getSystemService("audio")`.

## UI Design

- **Desktop:** Dropdown selects for microphone and speaker, with a "Refresh devices" button. Available in both the Preferences page (for defaults) and the active call overlay (for mid-call switching).

- **Android:** Three route buttons: Earpiece / Speaker / Bluetooth. The per-device dropdown is not shown because cpal device selection doesn't control Android routing. The buttons call `setCommunicationDevice` via JNI.

- **Active call overlay:** An "Audio" button reveals the device selector inline within the call card. On desktop, changes are applied immediately via stream hot-swap. On Android, there's a ~1 second switch delay.
