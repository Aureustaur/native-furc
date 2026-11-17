# Android port

This directory is a separate `eframe` NativeActivity app crate. It currently builds a UI shell; it does not connect to Furcadia or reuse the desktop client core yet.

## Build a sideloadable APK

The crate is configured for Android arm64 phones and x86_64 emulators. `cargo-apk` builds the Rust shared library and packages it as a debug-signed APK.

### One-time setup

1. Install Android Studio, then use **SDK Manager** to install Android SDK Platform 35, Build Tools 35.0.0, Platform Tools, and Android NDK 27.2.12479018 (side by side).
2. Set `ANDROID_HOME` to the SDK directory. If needed, set `ANDROID_NDK_HOME` to the installed NDK directory.
3. Install Rust 1.100.0 and the Android targets, then install `cargo-apk`:

   ```sh
   rustup toolchain install 1.100.0 --target aarch64-linux-android,x86_64-linux-android
   cargo +1.100.0 install cargo-apk --version 0.10.0 --locked
   ```

### Build and install

From the repository root:

```sh
cd android
cargo +1.100.0 apk build
find target -name 'furcadia-native*.apk'
adb install -r path/to/furcadia-native.apk
```

Alternatively, connect a device with USB debugging enabled (or start an emulator) and run:

```sh
cd android && cargo +1.100.0 apk run
```

The default development profile produces a debug-signed APK suitable for local installation and sideloading. To sideload without `adb`, copy the generated APK to the device and open it in Android's file manager. Android will prompt you to allow installs from that source if needed. A release-signed distribution needs a private signing key that you control.

Pushes or pull requests that change `android/` also run [the Android build workflow](../.github/workflows/android.yml), which uploads the APK as the `android-sideload-apk` workflow artifact. You can also start it manually from GitHub Actions.

## Port milestones

1. Build and launch the shell on a device or emulator.
2. Extract/configure the protocol, authentication, rendering, and asset code for reuse without compiling desktop-only code.
3. Implement touch-first sign-in and character selection.
4. Add the dream view, chat, whispers, and player actions in compact mobile navigation.
5. Replace desktop-only integrations with Android storage, audio, notifications, and update behavior.

The current repository environment has `adb`, but no Android SDK/NDK or Rust Android targets. Device installation also requires an accessible ADB device. Build and install commands above still need to be exercised on a configured Android machine.
