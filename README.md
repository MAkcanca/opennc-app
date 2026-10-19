# OpenNC desktop

Real-time noise cancellation for your microphone, powered by the open [OpenNC](https://github.com/MAkcanca/OpenNC) model.
Pick a microphone, send the clean audio to a virtual cable, and select the cable as the microphone in Discord, Zoom,
Teams, OBS or any other app. One small native binary, no runtime to install, about 1 ms of CPU per 10 ms of audio.

## Run

1. Install a virtual audio cable: [VB-Cable](https://vb-audio.com/Cable/) on Windows, [BlackHole](https://existential.audio/blackhole/)
   on macOS, or a PipeWire/PulseAudio null sink on Linux.
2. Download the model: `opennc-v1-stream.zip` from the [OpenNC v1 release](https://github.com/MAkcanca/OpenNC/releases/tag/v1),
   unpacked into `models/opennc-v1` next to the executable (or point `OPENNC_MODEL_DIR` at it).
3. Start `opennc`, choose your microphone and "CABLE Input" as the output, press the power button.
4. In your apps, choose "CABLE Output" as the microphone.

Settings live in `%APPDATA%\OpenNC\config.json` (`~/.config/OpenNC/config.json` elsewhere).

## Build

```
cargo build --release            # target/release/opennc (GUI) and opennc-cli
cargo test --release             # engine must match the Python reference on tests/data (needs models/opennc-v1)
opennc-cli devices               # list audio devices as the app names them
opennc-cli live <model_dir> "<mic>" "<output>" 10     # run the loop for 10 s and report dropped buffers
opennc-cli <model_dir> in.wav out.wav                  # offline processing, 48 kHz wav
```

Rust 1.85+. Dependencies: `cpal` (audio), `ort` (onnxruntime, downloads its binaries at build time), `realfft`, `slint` (UI).

## Layout

| path | contents |
|---|---|
| `src/engine.rs` | streaming engine: port of the OpenNC reference implementation, explicit recurrent state, silence gate |
| `src/audio.rs` | one thread owning both audio streams at 48 kHz / 10 ms, lock-free hand-off to the engine |
| `src/main.rs` | the window: config, device lists, meters |
| `ui/app.slint` | the interface (Slint), Inter font bundled |
| `src/bin/cli.rs` | offline and live checks |
| `tests/reference.rs` | engine must reproduce the Python engine's output (max difference 3e-5 measured) |

## License

Apache License 2.0. Model weights: see the OpenNC repository. Inter font: SIL Open Font License.
