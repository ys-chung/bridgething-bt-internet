# bridgething-bt-internet

Give a [bridgething](https://bridgething.com) Car Thing internet over Bluetooth from a Linux box, so it keeps working when your phone isn't around.

The Car Thing has no Wi-Fi. bridgething normally reaches the internet through the companion app on your phone, or through the desktop app over a USB cable. This project is a small headless gateway that does the same job over Bluetooth from any Linux machine with an adapter, such as a home server in the same room.

## How it works

The bridgething daemon on the Car Thing serves its gateway protocol on Bluetooth RFCOMM (service `dead0000-854d-408e-81f0-fb6147f918fd`, channel 1). The phone app connects there. `bt-gateway` dials the same channel and runs bridgething's own shared companion core (`bridgething-companion`, the code behind the phone and desktop apps) on that link. The Car Thing's HTTP, WebSocket and tunnel requests then go out through this machine.

The daemon can hold several gateways at once and elects a primary one. A gateway that is playing media wins; otherwise the most recent claim wins. When your phone is in range and playing, it takes over. When it leaves, this gateway carries the traffic.

## What works

- Webapps that need the internet: browser, weather, calendar, Home Assistant, and so on. The browser and weather were tested on device.

Not wired up yet: OTA updates and installs from the app store, which the phone and desktop apps drive themselves. Phone-only features (notifications, phone calls, your phone's now-playing) still need the phone. Throughput is limited by Bluetooth to about 1.5 Mbps.

## Requirements

- A Linux host with a Bluetooth adapter and BlueZ (`bluetoothd`) running.
- Docker with Compose.
- A Car Thing running bridgething. Tested with v0.13.1.

## Setup

### 1. Enable Bluetooth on the host

```sh
sudo systemctl enable --now bluetooth
bluetoothctl show   # should list a powered controller
```

### 2. Pair the Car Thing

Put the Car Thing in pairing mode, then on the host:

```sh
bluetoothctl
[bluetoothctl]> scan on              # wait for "Car Thing (SN: ...)", note its address
[bluetoothctl]> scan off
[bluetoothctl]> agent DisplayYesNo
[bluetoothctl]> default-agent
[bluetoothctl]> pairable on
[bluetoothctl]> pair AA:BB:CC:DD:EE:FF   # answer "yes" here and confirm the PIN on the Car Thing
[bluetoothctl]> trust AA:BB:CC:DD:EE:FF
[bluetoothctl]> pairable off
```

The pairing keys live in the host's `bluetoothd`, so the container doesn't need them.

### 3. Run it

```sh
cp .env.example .env    # set CARTHING_MAC to the address from step 2
docker compose up -d
docker compose logs -f
```

A working link logs `a peer link is up`. The gateway retries with backoff when the Car Thing is off or out of range.

## Security notes

- The container uses `network_mode: host`, because Linux only allows Bluetooth sockets in the host network namespace. It opens no listening ports, so host firewall rules such as ufw are unaffected.
- It runs as a non-root user with all capabilities dropped, `no-new-privileges` and a read-only root filesystem.
- It doesn't need the D-Bus socket or any device nodes.

## Configuration

| variable | default | |
| --- | --- | --- |
| `CARTHING_MAC` | (required) | Bluetooth address of the Car Thing |
| `RFCOMM_CHANNEL` | `1` | bridgething gateway channel |
| `GATEWAY_NAME` | `bt-gateway` | name reported to the device |
| `DATA_DIR` | `/data` | state, cache and host identity |
| `TZ` | `UTC` | timezone reported to the device |
| `RUST_LOG` | `info` | log filter |

## Building

```sh
docker build -t bt-gateway .
```

The bridgething crates are pinned by git revision in `Cargo.toml`. Bump `rev` to match the bridgething version on your device.

## License

MIT. bridgething itself is MIT-licensed by Joey Eamigh; this project isn't affiliated with it.
