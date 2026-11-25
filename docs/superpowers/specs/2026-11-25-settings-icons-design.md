# Settings screens: icons (design)

Date: 2026-11-25 · Size: small change · Status: approved

## Intent
The settings screens (connection form, AI settings) look plain and are hard to scan: the
connection form has no icons at all. Icons should make them read better and help people find
things, in the app's Apple-like style. The app has few features, so the change stays small.

## Goals
- Section titles carry a colored icon tile, like the switch rows of the AI settings.
- Choices in segmented controls carry a small icon before their label.
- Inputs for addresses, users, secrets and models carry a leading icon.

## Non-goals
- No layout change: the cards and field order stay as they are.
- No icons on field labels (kept plain, as Apple does) or on dialog buttons.
- No provider logos (Claude/GPT/Gemini stay text: trademarks).

## Approaches
| | Approach | Trade-off |
|---|---|---|
| A | Grouped list (macOS System Settings rows) | Most compact, but long values (keys) do not fit a row |
| **B (picked)** | Current cards + icons | Smallest change, long inputs stay comfortable; screen length unchanged |
| C | Left-side section tabs | No scrolling, but hides the whole picture and is the largest change |

The person picked B.

## Decisions
| Place | Connection form | AI settings |
|---|---|---|
| Section tile | 연결 (blue, monitor) · 접속 경로 (indigo, route) · 원격 PC (orange, globe) · 세션 (grey, sliders) | 제공자·모델 (blue, server: sparkles is already the dialog header's icon, so not repeated); the switch rows stay |
| Segmented icons | 종류: RDP monitor / SSH square-terminal / VNC screen-share · 방식: 직접 arrow-right / SSH 점프 server / VPN shield-check · 인증: 비밀번호 lock / 개인 키 key-round | Provider: text only |
| Leading input icons | 주소 globe · 사용자 user · 비밀번호 lock · 키 파일 file-key | 서버 주소 globe · 모델 cpu · API 키 key-round |

New Lucide icons (lucide-static 1.74.0, ISC, the pinned version): globe, cpu, user,
screen-share, arrow-right, file-key, folder. `scripts/fetch-icons.sh` lists every icon the app
uses (including those added on this branch).

## Left to the builder
Exact tile size and spacing (match `widgets::icon_tile` and the AI settings rows), and which
other inputs (VPN fields, shared folder) get a leading icon, by the same rules.
