# Remote Web Desktop

제어받는 PC에서 Rust 서버를 실행하고, 다른 기기의 웹 브라우저로 화면을 보면서 마우스와 키보드를 조작하는 원격 데스크톱입니다. 클라이언트 설치가 필요하지 않습니다.

Axum/Tokio 서버, 네이티브 화면 캡처, H.264/JPEG 바이너리 WebSocket 전송으로 구성합니다. 서버는 OpenH264·ScreenCaptureKit/XCap·Enigo를, 브라우저는 WebCodecs·Canvas와 내장 JPEG 디코더를 활용합니다. 프레임 확인 응답(ACK)과 최대 2프레임 파이프라인으로 느린 연결에서 오래된 화면이 계속 쌓이지 않도록 합니다. 코덱·FPS·화질·전송 해상도를 조절할 수 있습니다. 기본 전송 설정은 성능 우선인 **60 FPS 목표·화질 45·최대 너비 1280**이며, 실제 60 FPS 달성을 보장하는 값은 아닙니다. 실제 성능은 화면 크기, 화면 변화량, 호스트 CPU와 네트워크에 따라 달라집니다.

## 빠른 시작

제어받을 데스크톱에 [Rust stable](https://rust-lang.org/tools/install/)과 아래 운영체제별 빌드 도구를 설치합니다.

```sh
cargo build --release
./target/release/remote-web-desktop
```

콘솔에 표시된 접속 암호를 사용해 `http://127.0.0.1:8080`에 로그인합니다. `RWD_PASSWORD`를 지정하지 않으면 시작할 때 새 암호를 생성합니다. 서버를 재시작하면 로그인 세션은 모두 만료됩니다.

Makefile로 빌드와 실행을 함께 할 수도 있습니다. `make run`과 `make demo`는 기본적으로 `0.0.0.0:8080`에서 수신합니다.

```sh
make setup                        # OS별 빌드 의존성과 Rust 설치
make demo                         # 테스트 화면으로 실행
make run                          # 실제 데스크톱 서버 실행
make stop                         # 다른 터미널에서 현재 프로젝트의 서버 종료
make demo HOST=127.0.0.1 PORT=8081 # 수신 주소와 포트 변경
```

고정 암호는 `RWD_PASSWORD` 환경변수로 설정합니다. 다른 기기에서는 `http://<서버의 LAN IP>:8080`으로 접속합니다. 전체 명령은 `make help`로 확인할 수 있습니다.

`make stop`은 현재 프로젝트의 `target/release`, `target/debug` 서버와 macOS 앱 서버를 포트에 관계없이 모두 종료합니다. 실행 중인 서버가 없으면 성공으로 끝나며, 다른 경로에서 실행한 서버는 종료하지 않습니다. macOS·Linux에서는 `SIGTERM`으로 정상 종료를 요청해 눌린 입력을 해제합니다. Windows에서는 프로세스를 종료하므로 입력 해제를 포함한 정상 종료가 필요하면 실행한 터미널에서 `Ctrl+C`를 사용하세요.

`make setup` (`make deps`도 동일)은 macOS, Debian/Ubuntu(apt), Fedora(dnf), Arch Linux(pacman), Windows(winget)를 지원합니다. Rust stable과 Cargo 의존성, 네이티브 컴파일러·라이브러리·NASM을 설치합니다. Windows 빌드는 MSVC 툴체인을 사용합니다. Linux에서는 시스템 패키지 설치에 sudo가 필요하며, macOS의 Command Line Tools 설치 창이 뜨면 설치 완료 후 다시 실행하세요. Homebrew가 없으면 공식 설치 프로그램을 실행합니다.

Make가 아직 없는 환경에서는 저장소 루트에서 설치 스크립트를 직접 실행하세요.

```sh
# macOS / Linux
bash scripts/setup.sh
```

```powershell
# Windows: winget이 설치된 관리자 PowerShell에서 실행
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/setup.ps1
```

Windows 설치 후 새 터미널을 열고 `make demo`를 실행합니다. 설치 프로그램에서 재부팅을 요청하면 먼저 재부팅하세요. 화면 기록·손쉬운 사용 권한과 Linux X11 세션은 아래 운영체제 준비 항목에 따라 설정해야 합니다. Python 기반 검증 도구는 별도 개발 의존성인 `requirements-dev.txt`를 사용하며 `make setup`의 앱 빌드 의존성에는 포함되지 않습니다.

macOS에서 실제 화면을 제어할 때는 아래 [macOS 앱으로 실행](#macos-앱으로-실행) 절차를 사용하면 앱 이름으로 화면 기록·손쉬운 사용 권한을 관리할 수 있습니다.

다른 기기에서 접속할 때는 네트워크 인터페이스를 열어 실행합니다.

```sh
cargo run --release -- --host 0.0.0.0 --port 8080
```

같은 네트워크의 클라이언트에서 `http://제어받는-PC의-IP:8080`을 엽니다. 호스트의 방화벽에서 접속할 네트워크에 대해 TCP 8080을 허용해야 합니다. 인터넷에서는 아래 HTTPS 구성이나 신뢰하는 VPN을 사용하세요. 서버 자체는 HTTP를 제공하며, 평문 HTTP는 접속 암호와 화면 내용을 암호화하지 않습니다.

로그인 후 연결하면 원격 화면이 표시됩니다. 화면을 클릭하여 키보드 포커스를 주고 마우스·드래그·휠·키보드를 사용합니다. 한 번에 한 클라이언트만 제어할 수 있습니다. 연결을 종료하거나 로그아웃하면 제어 권한이 해제됩니다.

원격 화면에 키보드 포커스가 잡혔을 때 **Ctrl+Alt+Shift**를 함께 누르면 입력을 해제하고 로컬 도구 모음으로 돌아옵니다. 보기 전용 모드에서는 화면만 확인할 수 있습니다.

H.264는 브라우저가 WebCodecs의 AVC 디코딩을 지원할 때 사용할 수 있습니다. 일반적으로 HTTPS 또는 localhost에서 활성화되며, 지원되지 않는 브라우저와 평문 LAN 접속에서는 JPEG를 사용합니다. 브라우저에 따라 AVC 지원이 다르므로 클라이언트가 지원 여부를 확인합니다.

## 운영체제 준비

| 호스트 | 준비 사항 |
| --- | --- |
| macOS 12.3 이상 | Swift 5.9 이상을 포함한 Xcode/Command Line Tools, 화면 기록 권한, 손쉬운 사용 권한 |
| Windows | Rust MSVC 툴체인, Visual Studio C++ Build Tools, 로그인된 데스크톱 세션 |
| Linux | X11 그래픽 세션과 네이티브 개발 라이브러리 |

macOS에서는 먼저 `xcode-select --install`로 빌드 도구를 설치합니다. ScreenCaptureKit 라이브러리의 Swift 브리지를 함께 빌드하므로 `swift --version`으로 Swift 5.9 이상인지 확인합니다. 배포 앱은 macOS의 Swift 런타임을 사용하며 실행할 Mac에 Xcode가 필요하지 않습니다. 현재 macOS 빌드의 최소 지원 버전은 12.3입니다. 실제 화면을 제어하는 서버는 시작할 때 **화면 기록**과 **손쉬운 사용** 권한을 확인하고, 누락된 권한의 macOS 요청 창과 **시스템 설정 → 개인정보 보호 및 보안**을 자동으로 엽니다. 실행 앱에 권한을 허용한 뒤 서버를 다시 실행하세요. macOS 버전에 따라 화면 기록 항목의 이름이 다를 수 있습니다. 화면 기록 권한은 캡처에, 손쉬운 사용 권한은 마우스와 키보드 입력에 필요합니다. 입력 권한에 대한 배경은 [Enigo 권한 문서](https://github.com/enigo-rs/enigo/blob/main/Permissions.md)를 참고하세요.

이미 두 권한이 있거나 `--demo`·`--benchmark`로 실행하면 권한 창을 열지 않습니다. SSH·자동 실행 환경 등에서 자동 요청을 끄려면 `--no-permission-prompts`를 지정하세요(`make run ARGS='--no-permission-prompts'`). 기존 `--request-permissions` 옵션도 유지되며, 데모·벤치마크·자동 요청 비활성화 옵션과는 함께 사용할 수 없습니다. 권한을 아직 허용하지 않았거나 설정 창을 열지 못해도 웹 서버는 계속 실행하며, 실제 원격 제어 연결 시 누락된 권한을 오류로 안내합니다. Windows와 Linux에서는 자동 권한 요청 창을 열지 않고 기존 실행 방식을 유지합니다.

Windows에서는 [Rust의 MSVC 설치 안내](https://rust-lang.github.io/rustup/installation/windows-msvc.html)에 따라 C++ 빌드 도구를 설치합니다. 로그인된 사용자 세션에서 실행해야 하며, UAC 보안 데스크톱과 로그인 화면 제어는 지원하지 않습니다.

Debian/Ubuntu 계열에서는 다음 개발 패키지를 설치합니다.

```sh
sudo apt-get update
sudo apt-get install build-essential pkg-config libclang-dev nasm \
  libxcb1-dev libxrandr-dev libdbus-1-dev libxkbcommon-dev \
  libpipewire-0.3-dev libwayland-dev libegl-dev
```

[XCap의 Linux 빌드 요구 사항](https://github.com/nashaofu/xcap#linux-system-requirements)과 [Enigo의 플랫폼 지원](https://github.com/enigo-rs/enigo#features)을 참고했습니다. 이 앱의 Linux 입력 경로는 X11을 대상으로 합니다. **Wayland 세션은 지원하지 않습니다.** 로그인 화면에서 Xorg/X11 세션을 선택하고, 해당 사용자의 데스크톱 세션에서 실행하세요. SSH로 실행할 때는 올바른 `DISPLAY`와 X11 접근 권한이 필요합니다.

OpenH264의 C/C++ 소스는 Cargo 빌드에 포함됩니다. x86 계열에서는 빌드 전에 `nasm`을 설치하면 OpenH264의 어셈블리 최적화를 사용할 수 있습니다. Intel macOS에서는 `brew install nasm`으로 설치할 수 있습니다. [OpenH264 Rust 빌드·성능 안내](https://docs.rs/openh264/0.9.8/openh264/#faq)

## macOS 앱으로 실행

권한 요청과 앱 실행은 다음 명령으로 준비할 수 있습니다.

```sh
export RWD_PASSWORD='원하는-접속-비밀번호'
make macos-permissions
# macOS 화면 기록·손쉬운 사용에서 Remote Web Desktop을 허용한 뒤:
make macos-restart
```

두 명령은 기본적으로 `0.0.0.0:8080`을 사용하며 `HOST`와 `PORT`로 변경할 수 있습니다. `macos-permissions`는 빌드, 기존 앱 종료, 필요한 경우 패키징, 앱 자체의 macOS 권한 요청, Finder 열기를 수행하며 권한이 부족하면 서버가 설정을 엽니다. 일반 서버 실행과 Finder 앱 실행도 누락된 권한을 자동으로 요청하므로 이 명령은 필수가 아닙니다. 두 권한이 모두 없으면 화면 기록 설정부터 열리며, 같은 개인정보 보호 및 보안 메뉴에서 손쉬운 사용도 허용하세요. 권한 승인·관리자 인증은 사용자가 직접 수행하며 TCC 데이터베이스나 기존 권한은 수정하지 않습니다. 이미 허용한 항목과 현재 앱의 서명이 다르면 `+`에서 Finder에 표시된 현재 앱을 다시 선택하세요. `macos-restart`는 재빌드·재서명 없이 재시작합니다. 권한 요청은 Apple의 [화면 기록 권한 API](https://developer.apple.com/documentation/coregraphics/cgrequestscreencaptureaccess())와 [접근성 권한 API](https://developer.apple.com/documentation/applicationservices/1459186-axisprocesstrustedwithoptions)를 사용합니다.


터미널이나 IDE에서 단독 바이너리를 실행하면 macOS가 실행한 터미널·IDE를 개인정보 보호 권한의 대상으로 판단할 수 있습니다. [Apple의 TCC 설명](https://developer.apple.com/forums/thread/678819)은 권한이 책임 앱에 귀속되며 앱 번들의 주 실행 파일로 네이티브 실행 파일을 사용할 것을 설명합니다. 이 프로젝트의 패키징 도우미는 기존 Rust 서버를 네이티브 실행 파일이 있는 `.app`으로 묶습니다.

```sh
cargo build --release
scripts/package-macos.sh
scripts/package-macos.sh start --host 127.0.0.1 --port 8080
```

생성되는 `target/Remote Web Desktop.app`을 Finder에서 열어도 됩니다. Finder 실행은 기본값인 `127.0.0.1:8080`을 사용하며, 다른 기기에서 접속하려면 `scripts/package-macos.sh start --host 0.0.0.0 --port 8080`처럼 서버 인자를 넘깁니다. 일반 앱 창이나 Dock 아이콘은 표시되지 않지만 권한이 부족하면 macOS 권한 요청 창과 시스템 설정을 엽니다. 시스템 설정에서 **Remote Web Desktop**에 화면 기록·손쉬운 사용 권한을 부여하고, 권한을 변경한 뒤 앱을 종료하고 다시 시작합니다.

시작 상태·오류·자동 생성 암호는 `~/Library/Logs/Remote Web Desktop/server.log`에 기록됩니다. 이 로그는 현재 사용자만 읽고 쓸 수 있도록 권한 `0600`으로 생성되며 새 실행 때 초기화됩니다. 로그를 개인적으로 열어 현재 암호를 확인하고, 암호가 포함된 내용을 공유하지 마세요. `start`는 설정된 `RWD_PASSWORD`를 앱으로 전달합니다. 지정하지 않은 경우에는 로그의 생성 암호를 사용하세요.

```sh
scripts/package-macos.sh status
scripts/package-macos.sh log-path
scripts/package-macos.sh stop
```

`status`는 실행 프로세스와 로그 경로를, `log-path`는 경로만 출력합니다. `stop`은 정상 종료를 요청해 눌린 입력을 해제합니다. 실행 인자를 변경하거나 새 바이너리로 패키징하려면 먼저 `stop`을 실행합니다. 패키징은 이미 빌드된 release 바이너리를 사용하며 자동 시작 서비스는 설치하지 않습니다. 로컬 임시 서명을 사용하므로 앱을 다시 빌드·패키징한 뒤에는 macOS 권한을 다시 허용해야 할 수 있습니다. 앱 런처는 ScreenCaptureKit의 각 시작·종료 완료 대기를 기본 5초로 제한합니다(`SCREENCAPTUREKIT_COMPLETION_TIMEOUT_SECS`, 명시된 운영자 값은 유지). 첫 프레임 대기는 별도로 최대 1초이며, 정상 스트리밍 중에는 다음 화면 갱신을 기다리지 않고 최신 샘플을 읽습니다.

## 접속 설정

```sh
./target/release/remote-web-desktop --help
```

| 설정 | 기본값 | 설명 |
| --- | --- | --- |
| `--host` / `RWD_HOST` | `127.0.0.1` | 서버가 수신할 IP 주소 |
| `--port` / `RWD_PORT` | `8080` | HTTP/WebSocket 포트 |
| `RWD_PASSWORD` | 시작할 때 생성 | 접속 암호. 직접 지정할 때 UTF-8 기준 1–1024바이트 (12바이트 이상 권장) |
| `--demo` | 꺼짐 | 테스트 화면을 생성하고 실제 데스크톱 입력을 수행하지 않음 |
| `--public-origin` / `RWD_PUBLIC_ORIGIN` | `http://` + 요청 Host | HTTPS 프록시를 사용할 때 외부 접속 주소 지정 |
| `--secure-cookie` | 꺼짐 | 세션 쿠키를 HTTPS에서만 전송 |
| `--codec` | `jpeg` | 초기 인코더: `jpeg`, `image`, `h264`. 브라우저에서 변경 가능 |
| `--capture` / `RWD_CAPTURE_BACKEND` | `auto` | macOS는 ScreenCaptureKit을 우선하고 실패 시 XCap으로 전환. `sck`는 실패를 그대로 보고하며 `xcap`은 이전 캡처 경로 고정 |
| `--h264-mode` / `RWD_H264_MODE` | `video` | OpenH264의 빠른 video 모드와 화질에 따른 QP 하한. `screen`은 이전 화면 공유 모드·QP 정책 비교용 |
| `--resize` | `fast` | `fast`: SIMD bilinear, `image`: 비교용 bilinear, `nearest`: 더 거친 최근접 축소. `fast`와 `nearest` 모두 `fast_image_resize` 사용 |
| `--benchmark N` | 꺼짐 | JPEG 두 경로·H.264 두 모드와 축소 세 경로를 비교하고 종료 |

고정 암호가 필요한 경우 환경변수로 지정합니다. 암호는 프로젝트 파일에 넣지 말고 서비스 환경변수나 비밀 저장소로 관리하세요.

```sh
export RWD_PASSWORD='여기에-12자-이상의-고유한-암호'
./target/release/remote-web-desktop --host 0.0.0.0
```

[.env.example](.env.example)은 환경변수의 예시입니다. 서버가 `.env` 파일을 자동으로 읽지는 않습니다. Windows PowerShell에서는 `$env:RWD_PASSWORD = '고유한-긴-암호'`로 지정합니다.

암호는 로그인 시에만 전송하고 이후에는 `HttpOnly`, `SameSite=Strict` 세션 쿠키를 사용합니다. 세션 유효 기간은 8시간입니다. 로그인·로그아웃과 WebSocket 연결은 허용된 `Origin`을 검사합니다. 로그인 시도는 서버가 보는 IP당 분당 10회로 제한됩니다. 프록시 뒤에서는 프록시 IP를 공유하므로 이 한도가 합산됩니다. 로그아웃은 해당 세션을 폐기하고 연결된 제어 세션을 종료합니다.

## HTTPS로 운영

호스트의 도메인과 HTTPS 인증서를 준비하고, Rust 서버 앞에 역방향 프록시를 둡니다. [Caddy](https://caddyserver.com/docs/caddyfile/directives/reverse_proxy)는 WebSocket 업그레이드를 지원합니다.

```sh
./target/release/remote-web-desktop \
  --host 127.0.0.1 --port 8080 \
  --public-origin https://remote.example.com \
  --secure-cookie
```

[deploy/Caddyfile.example](deploy/Caddyfile.example)의 도메인을 실제 도메인으로 바꾸고 실행합니다.

```sh
caddy run --config deploy/Caddyfile.example --adapter caddyfile
```

도메인의 DNS는 프록시가 실행되는 호스트를 가리켜야 합니다. 외부에서 HTTPS 인증서 발급과 접속에 필요한 포트에 도달할 수 있어야 합니다. 브라우저에서는 `https://remote.example.com`으로 접속합니다. `--public-origin`에는 브라우저 주소와 같은 스킴·호스트·포트를 지정하며 경로를 넣지 않습니다. 인터넷으로 노출하는 경우 Rust 서버의 8080 포트를 직접 열지 말고 프록시를 통해 접속하도록 구성하세요.

이 서버에는 NAT 탐색, 중계 서버, 계정 관리, 다중 사용자 권한 관리가 포함되지 않습니다. 공유기 뒤에 있는 호스트에 외부에서 접속하려면 별도로 VPN 또는 네트워크 라우팅을 구성해야 합니다.

## 구조와 성능

```text
브라우저
  ├─ HTTP: 정적 UI, 로그인, 세션 확인
  └─ WebSocket: H.264/JPEG 화면 ← / ACK·마우스·키보드 →
       Rust Axum/Tokio 서버
         ├─ 캡처/입력 작업 스레드와 별도의 인코딩 스레드
         ├─ macOS ScreenCaptureKit: OS 단계 축소·비동기 캡처·최신 샘플 1개
         ├─ XCap: 다른 OS의 캡처 및 macOS 비교·대체 경로
         ├─ Enigo: 운영체제 입력 / Windows API 바인딩: 포인터 좌표
         └─ OpenH264 / jpeg-encoder / image: 영상·이미지 인코딩
```

캡처·이미지 변환·인코딩과 운영체제 입력 작업을 비동기 HTTP 처리와 분리합니다. 인코딩은 캡처·입력과 별도 스레드에서 수행하므로 인코딩 때문에 입력 처리가 기다리지 않습니다. 브라우저가 프레임을 처리한 후 ACK를 보내면 다음 프레임의 전송 여유가 생깁니다. 전송 대기 프레임은 최대 2개입니다. 바이너리 전송을 사용하므로 Base64 변환에 따른 크기 증가가 없습니다.

macOS의 기본 캡처는 [ScreenCaptureKit Rust 라이브러리](https://docs.rs/screencapturekit/11.1.1/screencapturekit/)를 사용합니다. OS에 목표 해상도와 FPS를 전달해 전체 Retina 이미지의 CPU 복사·후속 축소를 줄입니다. Rust 측에는 최신 정상 샘플 하나만 유지하며, CoreVideo 버퍼를 읽을 때 행 간격을 확인하고 BGRA를 RGBA로 복사합니다. OS 캡처 큐는 3개 표면으로 제한하고 연결 종료 시 스트림을 닫습니다. 정지 화면은 마지막 정상 샘플을 재사용하므로 전송 FPS와 새로 캡처된 프레임의 FPS를 구분해야 합니다. `--capture sck`는 실패 시 XCap으로 전환하지 않아 성능 비교에서 실제 경로를 확정할 수 있습니다.

입력은 Enigo 0.6.1을 재사용하며 macOS에서 실기로 발견한 문제만 [로컬 패치](vendor/enigo/RWD_PATCH.md)로 보완했습니다. 업스트림의 좌표·버튼 상태 관리 방식을 적용해 빠른 이동 직후 클릭이 이전 위치에 전달되는 문제를 수정하고, 떨어진 위치의 연속 클릭을 더블클릭으로 묶지 않도록 했습니다. 입력마다 대기를 누적하던 종료 처리도 마지막 이벤트 이후 최대 60ms의 요청 유예로 바꿨습니다. 입력 처리 중에는 대기를 추가하지 않습니다. 종료 정책의 이전/수정 구현 비교는 [비교 결과](docs/benchmarks/macos-input-drain-benchmark.txt)에 있으며, 이는 실제 화면 전송 지연이나 OS 입력 처리 지연을 측정한 결과가 아닙니다.

| 코덱 | 서버 | 브라우저 | 용도 |
| --- | --- | --- | --- |
| `h264` | OpenH264, 기본 video 모드, Baseline, YUV 버퍼 재사용 | WebCodecs `VideoDecoder` | 프레임 사이의 변화를 압축해 대역폭 절감 |
| `jpeg` | `jpeg-encoder`, SIMD 기능, 4:2:2 | `createImageBitmap`/이미지 디코더 | 범용 호환 기본값 |
| `image` | `image::JpegEncoder`, 4:2:2 | JPEG와 동일 | 기본 라이브러리 구현과 성능 비교 |

H.264 SPS에서 실제 프로파일·레벨을 읽어 클라이언트에 전달하고, Annex B 데이터를 WebCodecs에 전달합니다. 이는 [WebCodecs AVC 등록 문서](https://www.w3.org/TR/webcodecs-avc-codec-registration/)의 Annex B 디코딩 방식을 따릅니다. 서버의 H.264 인코딩은 소프트웨어 방식이며 브라우저 디코딩의 하드웨어 사용 여부는 브라우저와 기기에 달려 있습니다. JPEG의 SIMD 최적화는 [jpeg-encoder](https://github.com/vstroebel/jpeg-encoder#crate-features)가 지원하는 CPU에서 동작합니다.

느린 연결에서는 H.264를 선택하거나 전송 해상도·화질을 먼저 낮추고, 화면이 자주 변하는 작업에서는 FPS를 조절하세요. 큰 화면과 높은 화질은 CPU 사용량과 대역폭을 늘립니다. JPEG는 매번 전체 이미지를 전송하므로 동영상, 게임, 고해상도 환경에는 비용이 큽니다. 현재 구현은 WebRTC, 하드웨어 영상 인코딩, 오디오 전송을 제공하지 않으며 Chrome Remote Desktop과 같은 성능을 보장하지 않습니다.

브라우저나 운영체제가 예약한 일부 키 조합은 가로챌 수 없습니다. 클립보드 동기화, 파일 전송, 잠금 화면 제어, 부팅 직후 무인 서비스는 구현 범위에 포함되지 않습니다.

## 성능 비교

```sh
cargo run --release -- --benchmark 60
```

같은 1280×720 RGBA 화면에서 창이 움직이는 장면을 각 인코더에 순서대로 입력합니다. 별도 워밍업 후 새 스트림을 시작하며 첫 키프레임 비용도 포함합니다. 출력은 CSV 형식으로 인코딩 평균·p50·p95 밀리초, 프레임 평균 바이트 수, 인코딩 처리량을 보여 줍니다. 각 코덱의 첫 출력은 라이브러리 디코더로 다시 읽어 크기를 검증합니다.

이어서 `image`와 `fast_image_resize`의 1920×1080 → 1280×720 축소를 같은 불투명 RGBA 화면으로 비교합니다. `image`/`fast`는 bilinear 필터를, `nearest`는 품질을 낮춘 최근접 필터를 사용합니다. 결과 크기와 RGB 평균 절대 오차도 확인합니다. 축소 비교는 최대 1,000회로 제한하며 입력 복사 시간은 측정에서 제외합니다. `--resize fast`는 CPU에 맞는 SIMD 경로를 선택하는 기존 라이브러리를 사용합니다. `nearest`도 같은 라이브러리의 최근접 필터로 비교하며, bilinear와 출력 화질이 다릅니다. SCK가 이미 목표 크기로 캡처한 프레임에는 후속 CPU 축소를 건너뛰므로 `--resize` 선택에 따른 차이가 없습니다.

측정에는 색 공간 변환과 인코딩이 포함되고 화면 캡처, 축소, 네트워크, 브라우저 디코딩·표시는 제외됩니다. 따라서 출력의 `encode_fps`는 실제 원격 화면 FPS가 아닙니다. JPEG 두 구현은 화질 70과 4:2:2를 사용하지만, H.264는 4:2:0 및 프레임 간 압축을 사용합니다. H.264의 화질 값은 QP 범위로 매핑되므로 같은 숫자가 같은 시각적 품질을 뜻하지 않습니다. 데스크톱용 합성 장면의 결과이며 동영상이나 사진의 압축률을 대표하지 않습니다.

해당 호스트에서만 실행할 바이너리는 `RUSTFLAGS='-C target-cpu=native' cargo build --release`로 CPU별 최적화를 추가할 수 있습니다. 이렇게 만든 바이너리는 다른 CPU에서 실행되지 않을 수 있으므로 배포용 기본 빌드와 구분하세요.

브라우저에서도 **성능 비교 → 30회 비교 실행**으로 `createImageBitmap`과 `HTMLImageElement`를 비교할 수 있습니다. 두 경로는 같은 JPEG 샘플을 사용하고 각각 3회 워밍업 후 30회 측정한 중앙값을 보여 줍니다. H.264 연결 중에는 현재 Canvas 화면을 JPEG로 변환해 공통 샘플로 사용합니다. **빠른 방식 적용**을 누르면 결과를 적용하고 이 브라우저에 설정을 저장합니다.

브라우저 비교값은 디코딩과 Canvas 그리기 제출까지의 시간입니다. 네트워크, 호스트 인코딩, 실제 화면 표시 완료는 포함하지 않습니다. 하단 상태 표시의 실측 FPS·RTT·대역폭은 연결 상태를 보여 주며, H.264의 렌더 시간에는 디코더 출력 대기가 포함되므로 JPEG의 렌더 시간과 순수 디코딩 속도를 직접 비교할 수 없습니다.

2026-10-29, **Apple M5 Pro · RAM 48 GiB · macOS 26.6.2**, release 빌드에서 90프레임을 측정했습니다. 아래는 최적화 후 라이브러리 옵션 비교이며 화면 캡처를 포함하지 않습니다. [전체 결과](docs/benchmarks/encoder-resize-optimized.txt), [최적화 이전 결과](docs/benchmarks/benchmark.txt)

| 인코더 / 설정 | 평균 인코딩 | p95 | 평균 페이로드 |
| --- | ---: | ---: | ---: |
| `image` JPEG | 3.940 ms | 4.058 ms | 89,650 B |
| `jpeg-encoder` JPEG | 3.756 ms | 3.813 ms | 73,733 B |
| OpenH264 `screen` | 4.986 ms | 5.884 ms | 4,319 B |
| OpenH264 `video` | 1.812 ms | 2.055 ms | 1,399 B |

`video`는 OpenH264의 실시간 영상 모드를 사용하고 낮은 화질 설정에서 QP 하한을 올려 세부 표현을 줄입니다. 위 차이는 모드와 양자화 정책을 함께 바꾼 결과이며 동일 화질 비교는 아닙니다. CPU 스레드 수를 늘리는 것만으로 해결되지 않아, 라이브러리 옵션을 측정한 뒤 기본값을 선택했습니다.

| 축소 구현 / 필터 | 평균 | p95 | bilinear 대비 RGB 평균 절대 오차 |
| --- | ---: | ---: | ---: |
| `image` / bilinear | 7.701 ms | 7.780 ms | 기준 |
| `fast_image_resize` / bilinear | 1.560 ms | 1.591 ms | 0.000425 / 255 |
| `fast_image_resize` / nearest | 0.169 ms | 0.179 ms | 1.730377 / 255 |

최근접 축소는 이 장면에서 SIMD bilinear보다 약 9.2배 빨랐지만 글자·대각선이 거칠어질 수 있습니다. 기본값은 `fast`를 유지하며 추가 품질 저하를 허용하면 `--resize nearest`를 선택할 수 있습니다. 이 비교는 SCK 자체의 OS 축소 필터 성능을 측정하지 않습니다.

새 기본 전송 해상도에 가까운 **1280×830·화질 45·60 FPS 목표·120프레임**에서도 OpenH264 옵션을 비교했습니다. [전체 옵션별 결과](docs/benchmarks/encoder-tuning-1280-q45.txt)

| 옵션 | 평균 / p95 인코딩 | 평균 페이로드 | 복원 휘도 PSNR |
| --- | ---: | ---: | ---: |
| 이전 `screen`, QP 하한 16 | 5.765 / 6.719 ms | 3,969 B | 51.89 dB |
| `video`, QP 하한 16 | 3.156 / 3.525 ms | 4,558 B | 52.06 dB |
| 채택한 `video`, QP 하한 29 | 2.268 / 2.358 ms | 572 B | 45.74 dB |

채택한 설정은 이 합성 장면에서 이전 설정보다 인코딩이 약 2.5배 빨랐고 평균 페이로드가 약 86% 작았습니다. 이는 세부 표현 손실을 허용한 결과입니다. PSNR도 이 장면의 휘도 복원값으로, 사진·게임·작은 글자의 체감 품질을 보장하지 않습니다. 각 조합은 워밍업 포함 125프레임을 디코딩해 유효성을 확인했고, 디코딩 검증은 인코딩 시간에서 제외했습니다. 재현 명령은 다음과 같습니다.

```sh
cargo run --release --example encoder_tuning -- 120 1280 830 45
```

### 클라이언트 성능 옵션 비교

브라우저의 **성능 우선**(`fast`)은 60 FPS·화질 45·너비 1280이고, **고해상도**(`fast_hq`)는 이전 60 FPS 설정인 화질 65·너비 1600을 보존합니다. 같은 캡처·인코더 설정에서 두 프리셋을 비교하면 해상도·화질을 낮춘 효과를 분리할 수 있습니다.

JPEG는 기본적으로 WebSocket의 Blob을 내장 디코더에 바로 전달해 매 프레임 ArrayBuffer에서 Blob을 새로 만드는 경로를 줄입니다. **성능 비교 → JPEG 수신 방식**에서 `Blob 직접 디코딩`과 `ArrayBuffer 비교 경로`를 바꿀 수 있습니다. JPEG 디코더는 같은 화면 샘플을 사용한 30회 비교 후 선택합니다. 이 짧은 디코더 비교는 WebSocket 수신 방식의 비용을 측정하지 않으므로 Blob/ArrayBuffer 비교에는 실제 스트리밍 리소스 측정을 사용합니다. H.264는 두 설정에서 모두 ArrayBuffer를 사용합니다.

Canvas는 기본적으로 `desynchronized: true`를 요청해 브라우저의 지연 감소 경로를 사용합니다. 지원하지 않는 브라우저는 힌트를 무시할 수 있으며 GPU 완료를 기다리는 옵션은 아닙니다. `http://127.0.0.1:8080/?canvas=sync`로 열면 이 힌트를 끈 비교 경로를 사용합니다. `?jpegTransport=arraybuffer&canvas=sync`로 두 이전 경로를 함께 선택할 수도 있습니다. Canvas 옵션은 페이지를 새로 열 때 적용됩니다.

Chrome 156 데모 검증에서 두 JPEG 수신 방식, H.264 전환, `fast` 해상도, Canvas 힌트의 활성/비활성, 재연결·로그아웃이 통과했습니다. [클라이언트 검증 결과](docs/benchmarks/client-optimization-browser-results.json)에는 JPEG 디코더 중앙값 1.40 ms(ImageBitmap), 0.90 ms(HTMLImageElement)가 기록되어 있습니다. 이 값은 한 데모 샘플의 디코딩·그리기 제출 시간이며, 클라이언트 최적화 전체의 실기 성능 향상 수치는 아닙니다.

실제 이동 화면에서도 **SCK·JPEG·1280×830·화질 45·목표 60 FPS**를 고정하고 네 조합을 비교했습니다. 각 조건은 새 headless Chrome 156을 실행해 3초 워밍업 후 10초간 측정했으며 아래 실기 측정과 같은 Mac·루프백 환경을 사용했습니다. Canvas 열은 실제 적용된 `desynchronized` 값입니다.

| JPEG 수신 / 원본 보고서 | Canvas 힌트 | 그리기 FPS | Chrome CPU | 수신→그리기 중앙값 / p95 |
| --- | --- | ---: | ---: | ---: |
| [ArrayBuffer](docs/benchmarks/resource-client-arraybuffer-sync.json) | 꺼짐 | 53.89 | 37.98% | 3.7 / 4.7 ms |
| [Blob](docs/benchmarks/resource-client-blob-sync.json) | 꺼짐 | 53.89 | 37.31% | 3.5 / 4.3 ms |
| [ArrayBuffer](docs/benchmarks/resource-client-arraybuffer-desync.json) | 켜짐 | 53.90 | 38.14% | 3.8 / 4.7 ms |
| [Blob (기본)](docs/benchmarks/resource-client-blob-desync.json) | 켜짐 | 53.99 | 37.77% | 3.5 / 4.3 ms |

네 조합 모두 오류 없이 완료했습니다. Blob 경로에서 수신→그리기 중앙값이 0.2–0.3ms 낮게 관측됐지만 FPS·CPU 차이는 작고 조건당 한 번의 짧은 측정이므로 유의한 개선으로 단정하지 않습니다. Canvas 힌트의 성능 이득도 이 측정으로 입증되지 않았습니다. 수신→그리기는 브라우저 메시지 도착부터 `drawImage` 반환까지이며 GPU 표시 완료나 전체 원격 입력 지연을 포함하지 않습니다. 두 비교 옵션은 계속 선택할 수 있습니다.

### 실제 화면 전송의 리소스 측정 — 최적화 후

2026-10-29, 앱의 macOS 권한을 갱신한 뒤 **ScreenCaptureKit 실제 화면 캡처와 H.264/JPEG 브라우저 재생·리소스 측정을 완료했습니다.** 아래 비교는 같은 release 앱과 새 클라이언트를 유지하고 서버의 캡처·인코더 옵션 및 전송 프리셋만 바꾼 결과입니다. 같은 SCK/video 앱에서 네이티브 검증 10개와 웹 클라이언트 검증 6개도 통과했습니다. 클릭·드래그·키보드·한글/이모지·스크롤·연결 종료 시 입력 해제·재접속을 확인했고, 웹 클라이언트의 8회 교차 클릭도 정확한 OS 좌표에 도달했습니다. 200회 연속 포인터 입력 후 연결 종료는 64.4ms였으며, 이 값은 입력 반응 지연이 아닙니다. [네이티브 검증 결과](docs/benchmarks/native-optimized-verification.json), [웹 클라이언트 검증 결과](docs/benchmarks/native-optimized-browser-results.json), [최종 검증 상태](docs/benchmarks/optimized-native-status.json)

**Apple M5 Pro · CPU 15코어 · RAM 48 GiB · macOS 26.6.2 · Chrome 156**, 내장 Retina 화면(논리 1512×982, 물리 3024×1964)에서 [이동 창 장면](scripts/performance-fixture.html)을 표시했습니다. 서버와 headless 클라이언트는 같은 Mac의 루프백 연결을 사용했으며, 각 조건은 3초 워밍업 후 10초간 측정했습니다. 모든 행은 목표 60 FPS입니다. `fast_hq`는 1600×1038·화질 65, 새 기본 `fast`는 1280×830·화질 45입니다. CPU 100%는 코어 하나이며 RSS는 초당 표본의 중앙값입니다.

H.264 비교:

| 캡처 / OpenH264 모드 | 프리셋 | 그리기 / 고유 캡처 FPS | 서버 CPU | 서버 RSS MiB | 수신 Mbps | Chrome CPU |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| XCap / screen | fast_hq | 28.15 / 28.19 | 67.73% | 161.38 | 4.137 | 17.03% |
| SCK / screen | fast_hq | 55.50 / 55.15 | 81.51% | 102.03 | 6.882 | 30.11% |
| SCK / video | fast_hq | 54.49 / 54.16 | 45.23% | 79.13 | 3.427 | 28.78% |
| XCap / screen | fast | 34.46 / 34.47 | 60.28% | 154.69 | 3.822 | 18.84% |
| SCK / screen | fast | 55.01 / 53.71 | 55.65% | 98.48 | 6.123 | 29.46% |
| **SCK / video** | **fast (새 기본)** | **54.48 / 53.64** | **45.49%** | **72.63** | **1.864** | **27.04%** |

원본: [XCap / screen](docs/benchmarks/resource-optimized-xcap-screen-motion.json), [SCK / screen](docs/benchmarks/resource-optimized-sck-screen-motion.json), [SCK / video](docs/benchmarks/resource-optimized-sck-video-motion.json). 각 보고서는 실행 파일 SHA-256·실행 옵션·실제 캡처 경로·출력 크기·브라우저 오류를 기록하며, 세 실행 모두 완료됐고 브라우저 오류는 없었습니다.

- 동일 `fast_hq`에서 XCap/screen → SCK/video는 **FPS 약 1.94배, 서버 CPU 약 33% 감소, 서버 RSS 약 51% 감소**였습니다. 출력 화질·색 공간·축소 방식이 완전히 같은 비교는 아닙니다.
- 캡처만 XCap → SCK로 바꾸고 screen 모드를 유지하면 28.15 → 55.50 FPS로 증가했지만 서버 CPU도 67.73 → 81.51%로 증가했습니다. 초당 인코딩하는 프레임 수가 다른 비교이며 캡처 변경만으로 CPU 사용량이 줄었다고 해석하지 않습니다.
- SCK에서 screen → video로 바꾸면 비슷한 54–55 FPS에서 서버 CPU가 약 44.5%, 전송량이 약 50.2% 감소했습니다. 실시간 인코더 모드와 양자화 정책을 함께 변경한 결과이며 동일 화질 비교는 아닙니다.
- SCK/video의 `fast_hq` → `fast`는 이번 실행에서 FPS·CPU 차이가 거의 없었고 **전송량이 약 45.6% 감소**했습니다. 저해상도·저화질의 주된 실측 이득은 대역폭과 일부 서버 메모리였습니다.
- 이전 서버 옵션의 `fast_hq`에서 새 기본 SCK/video/`fast`로 바꾸면 28.15 → 54.48 FPS, 서버 CPU 약 33% 감소, RSS와 전송량 각각 약 55% 감소였습니다. **목표 60 FPS에는 도달하지 않았습니다.**

JPEG 비교도 같은 실행에서 측정했습니다. OpenH264 모드 옵션은 JPEG에 영향을 주지 않습니다.

| 캡처 | 프리셋 | 그리기 / 고유 캡처 FPS | 서버 CPU | 서버 RSS MiB | 수신 Mbps | Chrome CPU |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| XCap | fast_hq | 35.95 / 35.99 | 58.67% | 144.70 | 40.072 | 36.46% |
| SCK | fast_hq | 55.00 / 54.33 | 44.99% | 77.27 | 62.689 | 40.79% |
| XCap | fast | 40.85 / 40.88 | 53.13% | 144.75 | 26.084 | 35.13% |
| SCK | fast | 55.00 / 54.37 | 45.60% | 67.41 | 36.106 | 38.25% |

같은 SCK/`fast`에서 H.264는 JPEG와 비슷한 FPS를 전달하면서 전송량이 약 94.8% 작았고 Chrome CPU도 27.04% 대 38.25%로 낮았습니다. JPEG는 SCK에서 전송 프레임 수가 늘어 총 Mbps도 증가했습니다. H.264의 XCap 28 FPS와 SCK 54 FPS 사이에서 Chrome CPU가 증가한 것도 서로 다른 처리량의 비교이므로 클라이언트 효율 저하로 단정할 수 없습니다.

그리기 FPS는 Canvas 제출 횟수이며 표시 완료 FPS는 아닙니다. 고유 캡처 FPS는 첫·마지막 서버 통계 사이에서 계산해 측정 구간이 약간 다르므로 그리기 FPS보다 소폭 높을 수도 있습니다. SCK의 이동 장면에서는 두 값이 가까워 높은 FPS가 주로 동일 샘플의 반복 전송으로 만들어진 것은 아닙니다. 서버 CPU/RSS에는 WindowServer·OS 캡처 서비스·GPU 자원이 포함되지 않으므로 전체 컴퓨터의 자원 절감 수치로 사용하지 않습니다. Chrome RSS는 공유 페이지 중복과 조건 순차 실행의 영향을 받아 코덱 단독 메모리 사용량을 뜻하지 않습니다. 짧은 단일 실행이며 반복 실험·장시간 안정성·실제 인터넷 지연·전력은 측정하지 않았습니다.

전송과 입력 검증을 마친 뒤 클라이언트가 없는 상태에서 4.24초간 측정한 서버 대기 사용량은 **CPU 0%, RSS 67.23 MiB**였습니다. 전송 후 남은 할당을 포함하며, 장시간 메모리 안정성 검사는 아닙니다. [최적화 후 전송 종료 상태](docs/benchmarks/resource-optimized-warm-idle.json)

### 실제 화면 전송의 리소스 측정 — 최적화 이전

**아래 표는 XCap 전체 해상도 캡처·OpenH264 screen 모드·이전 클라이언트로 측정한 최적화 이전 기준값입니다.** 기록 보존용이며 새 SCK 경로와 기본 1280 설정의 성능은 위의 최적화 후 실측 표를 참고하세요. 두 시점의 실행을 동일 조건의 반복 실험으로 취급하지 않습니다.

2026-10-29, 같은 **Apple M5 Pro · RAM 48 GiB · macOS 26.6.2**에서 실제 데스크톱 서버와 headless Chrome 156을 함께 실행해 측정했습니다. Retina 화면의 3024×1964 캡처를 **1600×1038**로 축소하고, 로컬 루프백으로 H.264/JPEG를 전송해 브라우저 Canvas에 그렸습니다. 각 설정은 3초 워밍업 후 10초 동안 측정했습니다. 15 FPS 프리셋은 화질 70, 30/60 FPS 프리셋은 화질 65를 사용합니다.

**CPU 100%는 코어 1개의 사용량**이며 전체 컴퓨터의 사용률이 아닙니다. CPU는 측정 구간의 사용자·시스템 CPU 시간 차이로 계산하고, RSS는 초당 표본의 중앙값을 표시합니다. 서버는 Rust 프로세스 하나, Chrome은 이 측정에서 새로 실행한 브라우저와 자식 프로세스 전체입니다. **Chrome의 RSS 합계는 공유 메모리를 중복 계산할 수 있어 실제 고유 물리 메모리 사용량과 다릅니다.**

정지 화면: [원본 측정값](docs/benchmarks/resource-usage-static.json)

| 코덱 / 목표 FPS | 실측 FPS | 수신 Mbps | 서버 CPU | 서버 RSS MiB | Chrome CPU | Chrome RSS 합계 MiB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| H.264 / 15 | 14.4 | 0.73 | 33.8% | 148.1 | 10.2% | 1038.6 |
| H.264 / 30 | 28.8 | 0.81 | 38.2% | 150.8 | 16.7% | 1044.3 |
| H.264 / 60 | 35.9 | 0.61 | 43.7% | 150.9 | 15.5% | 1042.5 |
| JPEG / 15 | 14.5 | 17.35 | 34.8% | 151.0 | 20.8% | 1079.5 |
| JPEG / 30 | 28.8 | 32.17 | 45.1% | 118.3 | 34.3% | 1122.4 |
| JPEG / 60 | 31.1 | 34.64 | 48.1% | 118.3 | 34.1% | 1133.3 |

이동 화면: 합성 창이 CSS 애니메이션으로 왕복하는 장면이며 동영상이나 게임을 대표하지 않습니다. [원본 측정값](docs/benchmarks/resource-usage-motion.json)

| 코덱 / 목표 FPS | 실측 FPS | 수신 Mbps | 서버 CPU | 서버 RSS MiB | Chrome CPU | Chrome RSS 합계 MiB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| H.264 / 15 | 14.6 | 2.50 | 36.0% | 149.0 | 11.8% | 1049.1 |
| H.264 / 30 | 27.4 | 4.23 | 61.5% | 151.6 | 17.4% | 1064.7 |
| H.264 / 60 | 30.5 | 4.44 | 62.6% | 151.6 | 18.9% | 1067.1 |
| JPEG / 15 | 14.6 | 17.44 | 33.3% | 151.7 | 20.9% | 1108.3 |
| JPEG / 30 | 28.7 | 32.04 | 46.2% | 115.9 | 34.3% | 1147.5 |
| JPEG / 60 | 35.6 | 39.73 | 57.2% | 115.9 | 37.2% | 1159.7 |

이 실행에서는 30 FPS 설정으로 약 27–29 FPS를 전달했습니다. 이동 화면의 H.264는 JPEG보다 대역폭과 브라우저 CPU 사용량이 작았고 서버 CPU 사용량은 더 컸습니다. **60 FPS는 설정 상한이며 이번 실측에서는 60 FPS에 도달하지 않았습니다.**

클라이언트가 없는 서버는 시작 직후 3.3초 측정에서 CPU 0%, RSS 11.2 MiB였습니다. 두 장면의 전송을 마친 뒤 연결을 종료하고 4.3초 측정한 값은 CPU 0%, RSS 115.8 MiB였습니다. 초기 대기와 전송 후 대기 메모리를 구분해야 하며, 이 짧은 측정으로 장시간 메모리 안정성을 판단할 수 없습니다. [시작 직후 대기](docs/benchmarks/resource-usage-idle.json), [전송 후 대기](docs/benchmarks/resource-usage-warm-idle.json)

실측 FPS는 성공한 Canvas 그리기 제출 횟수이며 물리 디스플레이의 표시 완료 횟수가 아닙니다. Mbps는 수신한 WebSocket 바이너리 페이로드만 계산해 TCP/TLS 등의 부가 바이트는 제외합니다. Chrome GPU 프로세스의 CPU/RSS는 포함하지만 GPU 하드웨어 사용률·GPU 메모리·WindowServer·화면을 생성하는 별도 브라우저는 측정하지 않았습니다. 같은 컴퓨터의 서버와 클라이언트가 자원을 공유하는 짧은 루프백 측정이므로 인터넷 지연이나 별도 기기의 성능을 나타내지 않습니다. 프리셋을 순서대로 측정해 캐시와 이전 할당의 영향을 받으며 코덱 간 시각적 품질 동등성도 검증하지 않았습니다.

### 최적화 후 실기 측정 방법과 지표 해석

새 캡처 경로의 실기 비교에는 화면 기록 권한을 부여한 앱을 명시적으로 SCK 모드로 실행합니다. 이미 실행 중이면 먼저 종료합니다. 다른 원격 제어 연결도 종료하고, 재시작한 서버의 새 암호를 사용합니다.

```sh
scripts/package-macos.sh stop
scripts/package-macos.sh start --capture sck --h264-mode video --resize fast
```

테스트 장면은 별도 터미널에서 다음과 같이 제공합니다.

```sh
python3 -m http.server 18084 --bind 127.0.0.1 --directory scripts
```

호스트의 보이는 Chrome에서 `http://127.0.0.1:18084/performance-fixture.html`을 열고 첫 모니터를 크게 채우도록 배치합니다. 확대율은 100%로 유지하고 다른 창으로 가리지 않습니다. `STATIC` 상태에서 다음 명령을 실행합니다.

```sh
export RWD_PASSWORD='서버의-실제-접속-암호'
uv run --with playwright --with psutil python scripts/resource_benchmark.py \
  --server http://127.0.0.1:8080 --seconds 10 --label static \
  --expected-capture sck --presets fast_hq fast \
  --output artifacts/resource-usage-sck-static.json
```

끝나면 보이는 테스트 페이지의 **Start motion**을 눌러 `MOTION` 상태로 바꾸고 같은 측정을 반복합니다.

```sh
uv run --with playwright --with psutil python scripts/resource_benchmark.py \
  --server http://127.0.0.1:8080 --seconds 10 --label motion \
  --expected-capture sck --presets fast_hq fast \
  --output artifacts/resource-usage-sck-motion.json
```

[측정 스크립트](scripts/resource_benchmark.py)는 macOS 패키지의 서버 PID를 자동으로 찾습니다. 다른 실행 방식에서는 `--server-pid PID`로 해당 서버 프로세스를 지정합니다. `--label`은 결과에 장면 이름을 기록하는 옵션이며 장면을 직접 전환하지 않습니다. 스크립트는 원격 OS 입력을 보내지 않고 headless 클라이언트의 전송 설정만 바꾸며, 이미지를 저장하지 않습니다. `--expected-capture sck`는 실제 캡처 경로가 다른 경우 측정을 실패 처리합니다. JSON에는 프레임별 브라우저 수신→그리기 시간과 서버의 캡처·인코딩 시간 표본도 남습니다.

| 지표 | 의미와 범위 |
| --- | --- |
| `rendered_fps` | Canvas `drawImage` 제출 횟수. 반복된 동일 캡처 샘플도 포함하며 실제 디스플레이 표시 완료 FPS는 아님 |
| `unique_capture_fps` | 실제 전송한 프레임에서 서로 다른 캡처 순번의 증가율. SCK 최신 샘플의 반복 전송은 한 번만 계산. 픽셀 차이 검사나 모니터 표시 FPS는 아님 |
| `capture_ms` | XCap은 동기 화면 캡처 호출 시간. SCK는 최신 비동기 샘플의 CPU 매핑·복사 시간이며, 앞서 OS에서 진행한 캡처·축소 시간은 포함하지 않음 |
| `capture_age_ms` | 선택한 SCK 샘플의 콜백 도착부터 CPU 복사 완료까지. 호스트 화면 변화부터 클라이언트 표시까지의 전체 지연이 아님 |
| `encode_ms` | 필요한 후속 CPU 축소·색 공간 변환·인코딩. SCK가 목표 해상도로 제공하면 후속 축소는 건너뜀 |

SCK의 정상(`complete`) 샘플에만 순번을 부여합니다. 정지 화면에서 SCK가 새 샘플을 만들지 않으면 그리기 FPS가 높아도 고유 캡처 FPS는 낮을 수 있습니다. XCap/데모는 매 동기 캡처를 한 프레임으로 계산합니다. 서버 통계는 약 1초마다 표본을 전송하므로 `capture_ms`/`encode_ms`는 모든 프레임의 분포가 아니고, 고유 캡처 FPS도 첫/마지막 통계 시점 사이의 값입니다.

**프로세스 CPU/RSS 감소만으로 전체 컴퓨터의 자원 절감을 단정하지 않습니다.** ScreenCaptureKit은 OS·GPU 쪽에 캡처·축소 작업을 맡깁니다. 현재 스크립트는 Rust와 측정용 Chrome 프로세스의 CPU/RSS를 수집하며 WindowServer, OS 캡처 서비스, GPU 하드웨어 사용률·메모리·전력은 집계하지 않습니다. GPU 프로세스의 CPU/RSS가 포함되어도 GPU 자체 사용량을 측정한 것은 아닙니다.

비교는 한 요소씩 바꾸는 것이 좋습니다. `fast_hq`를 고정하고 서버를 `--capture xcap --h264-mode screen --resize fast`로 재시작한 뒤 `--expected-capture xcap`으로 측정하면 이전 서버 옵션의 기준값을 얻을 수 있습니다. 캡처만 바꿀 때는 H.264 모드를 동일하게 유지하고, `nearest`는 XCap 경로에서 비교합니다. 이전 기록 파일을 덮어쓰지 않도록 `--output` 경로를 각각 지정합니다.

JPEG 수신 방식과 Canvas 힌트는 같은 서버를 유지하고 다음 명령의 `--client-query`만 달리해 비교할 수 있습니다. 빈 쿼리는 기본 Blob·Canvas 힌트, `jpegTransport=arraybuffer`는 수신 방식만 변경, `canvas=sync`는 Canvas 힌트만 끄는 비교입니다. 두 변경을 함께 적용하는 예시는 다음과 같습니다.

```sh
uv run --with playwright --with psutil python scripts/resource_benchmark.py \
  --expected-capture sck --presets fast --codecs jpeg --label motion \
  --client-query 'jpegTransport=arraybuffer&canvas=sync' \
  --output artifacts/resource-usage-client-comparison.json
```

측정 후 테스트 페이지 서버를 `Ctrl+C`로, 앱 서버를 `scripts/package-macos.sh stop`으로 종료합니다.

### 실제 동작 검증 — 최적화 이전 네이티브 경로

Chrome 156의 브라우저 테스트에서는 H.264 재생과 JPEG 전환, 디코더 실패 시 자동 복구를 확인했습니다. 당시 브라우저별 JPEG 비교값과 검증 결과는 [브라우저 검증 보고서](docs/benchmarks/browser-results.json)에 기록되어 있습니다.

같은 Mac에서 **Remote Web Desktop.app에 화면 기록·손쉬운 사용 권한을 부여한 후 실제 데스크톱 검증을 통과했습니다.** 내장 Retina 화면(논리 크기 1512×982, 배율 2)에서 JPEG 캡처·H.264 키프레임, 실제 클릭·드래그·키보드·한글/이모지·스크롤, 연결 종료 시 눌린 키와 버튼 해제, 200회 연속 입력 후 종료, 재접속까지 10개 항목을 확인했습니다. 200회 입력 후 연결 종료는 이 실행에서 61.9ms였습니다. 별도 headless Chrome 웹 클라이언트에서도 실제 화면의 H.264/JPEG Canvas 출력, 클릭, 8회 교차 클릭의 정확한 좌표, 드래그, 한글/이모지 전달 6개 항목을 확인했으며 브라우저 오류는 없었습니다. 결과는 [네이티브 검증 보고서](docs/benchmarks/native-verification.json)와 [웹 클라이언트 전체 경로 보고서](docs/benchmarks/native-browser-results.json)에 있습니다. 데스크톱 이미지는 보고서에 저장하지 않았습니다. Windows/Linux 및 다중 모니터는 실기 검증 전입니다.

**XCap 비교 경로**의 macOS 캡처는 디스플레이 색상 프로파일을 전송 영상에 보존하지 않습니다. 이 Mac에서는 일부 RGB 값의 차이가 관측됐으며, 위 검증은 화면과 입력의 정상 동작을 확인한 결과입니다. 색상 정확도가 필요한 작업에 대한 검증은 포함하지 않습니다. 새 **ScreenCaptureKit 경로는 sRGB 출력을 명시**하지만 실제 디스플레이의 정확한 색 재현은 별도 검증 전입니다.

## HTTP / WebSocket 프로토콜

| 경로 | 역할 |
| --- | --- |
| `GET /healthz` | 서버 상태: `{"ok":true}` |
| `POST /api/login` | `{"password":"..."}` 요청, 세션 쿠키 발급 |
| `GET /api/session` | `{"authenticated":true/false}` |
| `POST /api/logout` | 세션 폐기, 진행 중 제어 종료 |
| `GET /ws` | 인증 쿠키와 동일 Origin을 요구하는 WebSocket |

WebSocket은 처음에 `hello` JSON으로 호스트·모니터·전송 설정을 보냅니다. `settings` 응답은 새 설정이 적용되었음을 뜻합니다. JPEG 프레임은 바이너리 메시지 전체가 JPEG이고, H.264 프레임은 `RWH1` 4바이트 + 키프레임 여부 1바이트(`0` 또는 `1`) + Annex B 바이트입니다. H.264 설정은 프레임 직전 `{"type":"decoder","codec":"avc1.…"}`로 전달합니다. 프레임마다 클라이언트가 `{"type":"ack"}`를 보냅니다. 프레임을 임의로 버리면 H.264 참조 관계가 깨질 수 있으므로 순서대로 디코딩해야 합니다.

| 클라이언트 메시지 | 예시 |
| --- | --- |
| 전송 설정 | `{"type":"settings","monitor":1,"fps":30,"quality":70,"max_width":1600,"codec":"h264"}` |
| 포인터 이동 | `{"type":"pointer","x":0.5,"y":0.5}` |
| 마우스 버튼 | `{"type":"button","button":"left","down":true,"x":0.5,"y":0.5}` |
| 스크롤 | `{"type":"scroll","dx":0,"dy":-1}` |
| 키 입력 | `{"type":"key","code":"KeyA","key":"a","down":true}` |
| 텍스트 입력 | `{"type":"text","text":"안녕하세요"}` |
| 모든 눌린 입력 해제 | `{"type":"release"}` |
| 지연 측정 | `{"type":"ping","sent":1234.5}` → `pong`으로 같은 `sent` 반환 |

좌표는 선택한 모니터 내 0–1 범위입니다. FPS는 1–60, 화질은 30–90, 최대 너비는 640–2560, 스크롤은 축마다 -20–20, 텍스트는 최대 2,000자입니다. 잘못된 입력이나 시간 초과는 연결을 종료합니다. 인증 만료는 종료 코드 `4401`, 다른 제어자가 있을 때는 `4409`를 사용합니다. 모니터·인코더 설정 변경은 기존 프레임의 ACK를 받은 뒤 적용합니다.

## 검증

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

데모 모드는 화면 기록·입력 권한 없이 웹 UI와 전송 경로를 검증합니다.

```sh
export RWD_PASSWORD='demo-test-password-change-me'
cargo run --release -- --demo --host 127.0.0.1 --port 8080
```

다른 터미널에서 같은 암호를 지정하고 통합 테스트를 실행합니다. 테스트는 Python의 HTTP 표준 라이브러리와 [`websockets` 동기 클라이언트](https://websockets.readthedocs.io/en/stable/reference/sync/client.html)를 사용합니다.

```sh
export RWD_PASSWORD='demo-test-password-change-me'
uv run --with websockets python scripts/smoke_test.py http://127.0.0.1:8080
```

테스트는 인증·Origin 거부, 세션 쿠키, JPEG 프레임, 최대 2프레임 제한·ACK, H.264 협상과 JPEG 전환, 설정 변경, 입력 메시지, 단일 제어자 제한, 로그아웃에 따른 연결 종료, 재접속을 확인합니다. 실제 데스크톱에 입력을 보내지 않도록 `--demo` 서버에서만 동작합니다. 실제 캡처, Retina/배율 좌표 변환, 마우스·키보드 동작은 해당 운영체제에서 권한을 부여한 뒤 별도로 확인해야 합니다.

실제 브라우저 디코딩과 UI도 검증할 수 있습니다. 실행 중인 데모 서버와 같은 `RWD_PASSWORD`를 지정한 터미널에서:

```sh
python3 -m venv .venv
.venv/bin/python -m pip install -r requirements-dev.txt
.venv/bin/python scripts/browser_test.py http://127.0.0.1:8080
```

`uv`가 없는 환경에서도 위 가상환경을 준비한 뒤 `.venv/bin/python scripts/smoke_test.py`로 통합 테스트를 실행할 수 있습니다. 브라우저 테스트는 기본적으로 설치된 Google Chrome을 headless 모드로 사용합니다. Chrome이 없다면 `.venv/bin/python -m playwright install chromium` 실행 후 `PLAYWRIGHT_CHANNEL=chromium .venv/bin/python scripts/browser_test.py`로 실행합니다. Windows에서는 `.venv/bin/python` 대신 `.venv\Scripts\python.exe`를 사용합니다. 결과와 화면 이미지는 Git 추적에서 제외된 `artifacts/`에 저장됩니다. README에서 참조하는 과거 측정·검증 보고서는 `docs/benchmarks/`에 보존합니다. 테스트는 WebCodecs 지원 환경에서 H.264 디코딩, JPEG 전환, 입력 메시지, 보기 전용 모드, 디코더 비교, 작은 화면 배치, 재접속과 로그아웃을 확인합니다.

## 실제 데스크톱 검증 재현

[scripts/native_test.py](scripts/native_test.py)는 화면 캡처와 실제 운영체제 입력을 일회용 테스트 페이지로 검증합니다. **실제 마우스와 키보드를 조작하므로 검증 중에는 테스트 페이지를 전면에 두고 포커스를 유지해야 합니다.** 앞의 데모 서버를 종료하고 호스트의 화면 기록·입력 권한을 준비한 뒤 진행합니다.

1. 제어받는 호스트의 터미널에서 테스트 페이지 서버를 실행합니다.

   ```sh
   python3 scripts/native_test.py --serve
   ```

2. 같은 호스트의 눈에 보이는 Chrome 창에서 `http://127.0.0.1:18083`을 엽니다. 브라우저 확대율은 100%로 두고 클릭·드래그·텍스트·스크롤 영역이 선택한 모니터 안에 모두 보이도록 배치합니다. 페이지를 다른 창으로 가리지 않습니다.

3. 실제 데스크톱 서버를 실행합니다. 이 검증에는 `--demo`를 사용하지 않습니다. macOS에서는 위 앱 패키징을 마친 뒤 다음 명령을 사용하고, 앱의 비공개 로그에서 접속 암호를 확인합니다.

   ```sh
   scripts/package-macos.sh start --host 127.0.0.1 --port 8080
   ```

   단독 실행 파일을 사용하는 환경에서는 별도 터미널에서 아래처럼 실행하고 지정한 암호를 사용합니다.

   ```sh
   export RWD_PASSWORD='native-test-password-change-me'
   ./target/release/remote-web-desktop --host 127.0.0.1 --port 8080
   ```

4. `http://127.0.0.1:18083/layout`의 요소 위치와 `estimatedContentOrigin`을 참고해 **페이지 콘텐츠 영역의 좌상단 데스크톱 좌표**를 화면에서 직접 확인합니다. 주소 표시줄·창 테두리의 좌상단과 다릅니다. 추정값은 진단용이며 그대로 신뢰하지 않습니다. macOS에서는 논리 포인트 좌표를 사용하며 Retina 배율을 곱하지 않습니다. 이후 창 위치·브라우저 확대율을 바꾸면 좌표를 다시 확인합니다.

5. 아래 `X,Y`를 확인한 실제 좌표로, `--monitor 1`을 테스트 페이지가 있는 모니터 ID로 바꿉니다. 같은 암호를 지정한 터미널에서 실행하고 5초 대기 중 테스트 페이지로 전환해 포커스를 줍니다. 검증이 끝날 때까지 다른 창을 조작하지 않습니다.

   ```sh
   export RWD_PASSWORD='서버의-실제-접속-암호'
   sleep 5
   uv run --with websockets --with Pillow python scripts/native_test.py \
     --verify --server http://127.0.0.1:8080 \
     --fixture http://127.0.0.1:18083 --monitor 1 \
     --content-origin X,Y --report artifacts/native-results.json
   ```

`requirements-dev.txt`로 가상환경을 준비한 경우 `uv run --with websockets --with Pillow python` 대신 `.venv/bin/python`을 사용할 수 있습니다. 서버 포트나 테스트 페이지 포트를 바꾸었다면 `--server`, `--fixture`에도 같은 주소를 지정합니다.

서버는 한 클라이언트만 제어할 수 있으므로 검증 전에 다른 원격 제어 연결을 종료합니다. 기본 `--monitor 1`은 첫 모니터입니다. 다른 모니터를 선택하려면 원격 UI의 모니터 목록 순서(1부터)를 확인한 뒤 해당 UI의 연결을 종료하고 검증을 실행합니다.

검증은 JPEG를 Pillow로 디코딩해 예상 좌표의 네 영역을 자홍·녹색·흰색·파란색 계열로 분류하여 테스트 페이지가 실제로 캡처되었는지 확인하고, H.264의 설정·키프레임 수신을 확인합니다. 이어서 페이지가 받은 신뢰된 운영체제 이벤트를 통해 클릭·드래그·키 누름/해제·한글과 이모지 입력·스크롤·연결 종료 시 눌린 입력 해제·재접속을 검사합니다. 종료 직전 기본 200회의 포인터 입력을 테스트 페이지 내부로 연속 전송하고 종료 시간을 기록하여 누적 대기 회귀를 검사합니다(`--burst-events 0..300`). H.264 키프레임 수신 검사는 브라우저 디코딩 검사와 별개입니다.

색상 검사는 **페이지의 존재와 위치를 식별하기 위한 검사이며 정확한 색 재현을 검증하지 않습니다.** 디스플레이 색상 프로파일과 영상 변환 때문에 캡처 표본의 RGB 값이 CSS 값과 달라질 수 있습니다. 관측 RGB, 기준 RGB, 채널별 최대 차이와 색상 계열 판정은 진단용으로 보고서에 남깁니다.

테스트 서버의 `/state`, `/events`, `/layout`에서 현재 상태를 확인할 수 있습니다. `--report`는 이벤트와 프레임 크기·색상 표본 같은 검증 근거를 JSON으로 저장하며 데스크톱 이미지는 저장하지 않습니다. 실패하면 출력된 항목과 포커스·모니터·콘텐츠 원점·OS 권한을 확인한 뒤 재실행합니다.

웹 클라이언트까지 포함한 실제 제어 경로는 [scripts/native_browser_test.py](scripts/native_browser_test.py)로 검사합니다. 같은 테스트 페이지·서버·좌표를 사용하며, 별도의 headless Chrome이 원격 웹 클라이언트에 접속합니다. 이 검사도 실제 운영체제 입력을 보내므로 테스트 페이지의 포커스를 유지합니다.

```sh
sleep 5
uv run --with playwright --with Pillow python scripts/native_browser_test.py \
  --server http://127.0.0.1:8080 --fixture http://127.0.0.1:18083 \
  --monitor 1 --content-origin X,Y
```

검사는 실제 H.264 디코더 출력과 JPEG가 Canvas에 그려졌는지, 같은 색상 계열 분류기로 네 곳에서 테스트 페이지가 식별되는지 확인한 뒤 서로 떨어진 두 위치의 8회 교차 클릭마다 실제 OS 이벤트 좌표를 확인합니다. 드래그 및 텍스트 대화상자에서 보낸 한글·이모지가 호스트 테스트 페이지에 도달하는지도 확인합니다. 색 정확도 검사는 아닙니다. WebCodecs AVC를 지원하는 Chrome과 같은 `RWD_PASSWORD`가 필요합니다. 결과는 `artifacts/native-browser-results.json`에 메타데이터만 저장하며 스크린샷은 저장하지 않습니다.

검증을 마치면 테스트 페이지 서버와 직접 실행한 서버는 `Ctrl+C`로, macOS 앱 서버는 `scripts/package-macos.sh stop`으로 종료하고 테스트 페이지를 닫습니다.

## 문제 해결

| 증상 | 확인할 내용 |
| --- | --- |
| 화면이 검거나 캡처 오류가 발생함 | 호스트 화면 기록 권한, 활성 모니터, 로그인된 그래픽 세션 |
| 화면은 보이지만 입력이 안 됨 | macOS 손쉬운 사용 권한, 화면 포커스, Linux X11 세션 |
| 다른 기기에서 접속되지 않음 | `--host 0.0.0.0`, 호스트 IP, 방화벽, 같은 네트워크 또는 VPN |
| HTTPS에서 로그인·연결이 거부됨 | `--public-origin`과 브라우저의 실제 접속 주소가 일치하는지 확인 |
| 로그인 직후 다시 로그인 화면이 나옴 | HTTP에서 `--secure-cookie`를 켰는지 확인 |
| 이미 제어 중이라는 오류 | 기존 탭에서 연결 종료 후 다시 연결 |
| 프레임이 느림 | release 빌드 사용, 해상도·화질·FPS 조정, 호스트 CPU·네트워크 확인 |

`RUST_LOG=debug`를 설정해 서버 로그를 자세히 볼 수 있습니다. 시작 시 생성된 암호가 포함된 로그를 외부에 공유하지 마세요.
