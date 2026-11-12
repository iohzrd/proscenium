import { platform } from "@tauri-apps/plugin-os";

/**
 * On Android, make sure the app holds the microphone permission before native
 * audio capture starts. Capture runs in the Rust backend, which cannot ask for
 * the runtime permission; a `getUserMedia` call of the webview makes the app
 * request it. The stream is stopped at once. Rejects if the permission is
 * denied. On other platforms it does nothing.
 */
export async function ensureMicrophonePermission(): Promise<void> {
  if (platform() !== "android") return;
  let stream: MediaStream;
  try {
    stream = await navigator.mediaDevices.getUserMedia({ audio: true });
  } catch (e) {
    throw new Error(`Microphone permission is required: ${e}`);
  }
  for (const track of stream.getTracks()) track.stop();
}
