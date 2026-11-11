# HopDesk

SSH 점프 호스트(배스천)를 거쳐 사내망의 Windows PC에 원격 데스크톱(RDP)으로 접속하는 macOS 앱입니다.

터널을 따로 열고 원격 데스크톱 앱을 실행할 필요 없이, 저장한 연결을 고르고 **연결**을 누르면 됩니다.
HopDesk가 SSH 터널을 열고 저장된 계정으로 Windows에 자동 로그인합니다.

![연결 상세 화면](docs/screenshots/detail.png)

## 주요 기능

- **한 번에 접속:** 점프 호스트 SSH 로그인부터 Windows 로그인까지 자동으로 처리합니다.
- **안전한 비밀번호 보관:** 비밀번호와 개인 키 암호는 macOS 키체인에만 저장하고, 설정 파일에는 기록하지 않습니다.
- **호스트 검증:** 처음 접속할 때 SSH 호스트 키와 RDP 서버 인증서를 기억합니다(TOFU). 이후 바뀌면 접속을 막고 경고합니다.
- **진행 상황 표시:** `이 Mac → 점프 호스트 → 원격 PC` 경로 그림에 연결 단계가 표시되고, 실패하면 어느 구간에서 막혔는지 알려 줍니다.
- **알기 쉬운 오류 안내:** 오류 원문 대신 원인과 해결 방법을 한국어로 보여 줍니다.
- **자동 재연결:** 네트워크가 끊기면 SSH 채널을 다시 열어 재접속합니다.
- **SSH 인증:** 비밀번호 또는 개인 키(암호 포함) 방식을 지원합니다.
- **화면:** 라이트·다크 모드를 지원합니다. 해상도는 목록에서 고르거나 직접 입력합니다.

| 실패 안내 | 연결 추가 |
|---|---|
| ![실패 안내](docs/screenshots/failed.png) | ![연결 추가 폼](docs/screenshots/form.png) |

## 설치

미리 빌드된 배포판은 아직 없으므로 소스에서 빌드합니다.

필요한 것:
- macOS
- Rust 1.95 이상 — [rustup](https://rustup.rs)으로 설치

```sh
git clone https://github.com/softkr/HopDesk.git
cd HopDesk
scripts/bundle-macos.sh            # target/release/HopDesk.app 생성
scripts/bundle-macos.sh --install  # /Applications 에 설치
```

앱에는 임시(ad-hoc) 서명만 되어 있습니다. 다른 Mac으로 옮겨 처음 열 때 Gatekeeper가 막으면 Finder에서 우클릭한 뒤 **열기**를 선택하세요.

## 사용법

1. HopDesk를 실행하고 **새 연결 만들기**(⌘N)를 누릅니다.
2. 아래 항목을 입력하고 저장합니다(⌘S).
   - 점프 호스트의 주소, 사용자 이름, 인증 방식
   - 원격 PC의 주소. 점프 호스트에서 보이는 주소(예: `10.0.0.5`)를 입력합니다.
   - Windows 계정
3. 목록에서 연결을 고르고 **연결**(Enter)을 누릅니다. 원격 데스크톱은 별도 창으로 열립니다.

| 단축키 | 동작 |
|---|---|
| Enter | 연결 |
| ⌘N | 새 연결 |
| ⌘F | 검색 |
| ⌘E | 편집 |
| ⌘S | 저장 |
| Esc | 취소 |
| ↑ / ↓ | 목록 이동 |

### 명령줄

GUI 없이 터미널에서도 사용할 수 있습니다. 하위 명령 없이 실행하면 GUI가 열립니다.

```sh
hopdesk add office --jump ubuntu@bastion.example.com:22 --jump-key ~/.ssh/id_ed25519 \
    --target 10.0.0.5:3389 --user hong --domain CORP
hopdesk list
hopdesk connect office
hopdesk set-password office --rdp     # 저장된 비밀번호 변경 (--jump: 점프 호스트)
hopdesk forget bastion.example.com    # 기억한 호스트 키/인증서 삭제
hopdesk remove office
```

## 저장 위치

| 내용 | 위치 |
|---|---|
| 연결 설정 | `~/Library/Application Support/HopDesk/profiles.toml` |
| 기억한 호스트 키·인증서 | `~/Library/Application Support/HopDesk/trust.toml` |
| 비밀번호 | macOS 키체인, 서비스 이름 `HopDesk` |

두 설정 파일은 소유자만 읽을 수 있는 권한(0600)으로 저장됩니다.

## 동작 방식

```
HopDesk ──SSH──▶ 점프 호스트 ──direct-tcpip──▶ Windows PC :3389
           └──────────── TLS + CredSSP (RDP) ────────────┘
```

로컬 포트를 열지 않습니다. SSH `direct-tcpip` 채널을 RDP 연결의 전송 계층으로 바로 사용합니다.
TLS 인증서 검증과 CredSSP 인증은 실제 대상 호스트 이름을 기준으로 이루어집니다.

## 개발

```sh
cargo run                    # GUI 실행
cargo run -- connect office  # 명령줄로 접속
cargo clippy
HOPDESK_SHOTS=target/screenshots cargo test screenshots -- --ignored   # 화면 스냅숏 생성
```

| 경로 | 내용 |
|---|---|
| `src/main.rs` | 명령줄 인터페이스 |
| `src/tunnel.rs` | SSH 점프 호스트 터널 |
| `src/session.rs` | RDP 세션 |
| `src/trust.rs` | 호스트 키·인증서 검증 |
| `src/profile.rs` | 연결 설정과 키체인 |
| `src/gui/` | 런처 GUI (egui) |

GUI에서 연결할 때마다 `hopdesk connect <이름>`을 별도 프로세스로 실행하므로, 런처를 닫아도 열린 세션은 유지됩니다.

RDP 엔진은 [IronRDP](https://github.com/Devolutions/IronRDP)를 `third_party/`에 포함해 사용합니다.
SSH 채널을 전송 계층으로 쓰기 위한 작은 패치를 적용했으며, 자세한 내용은 [third_party/PATCHES.md](third_party/PATCHES.md)에 있습니다.

## 라이선스

포함된 IronRDP는 MIT 또는 Apache-2.0 라이선스를 따릅니다.
