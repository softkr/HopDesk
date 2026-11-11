SSH 점프 호스트를 거쳐 Windows PC에 원격 데스크톱(RDP)으로 접속하는 앱입니다.

## 다운로드

| 운영체제 | 파일 |
|---|---|
| macOS 11 이상 (Apple Silicon, Intel) | `HopDesk-{{VERSION}}-macos-universal.dmg` |
| Windows 10 / 11 (64비트) | `HopDesk-{{VERSION}}-windows-x64.zip` |
| Linux (x86_64, glibc 2.35 이상) | `HopDesk-{{VERSION}}-linux-x86_64.tar.gz` |

`SHA256SUMS.txt`로 내려받은 파일이 손상되지 않았는지 확인할 수 있습니다.

## 설치

**macOS**
1. dmg를 열고 HopDesk를 응용 프로그램 폴더로 끌어 놓습니다.
2. 처음 열 때 "확인되지 않은 개발자" 경고가 나오면 **시스템 설정 → 개인정보 보호 및 보안**에서 **그래도 열기**를 누릅니다.
   터미널을 쓴다면 `xattr -dr com.apple.quarantine /Applications/HopDesk.app` 를 실행해도 됩니다.

**Windows**
1. zip을 원하는 폴더에 풀고 `HopDesk.exe`를 실행합니다.
2. "Windows의 PC 보호" 창이 나오면 **추가 정보 → 실행**을 누릅니다.

**Linux**
```sh
tar xzf HopDesk-{{VERSION}}-linux-x86_64.tar.gz
cd HopDesk-{{VERSION}}-linux-x86_64
./install.sh      # ~/.local/bin 에 설치하고 앱 메뉴에 등록
```
비밀번호 저장에는 GNOME 키링이나 KWallet 같은 Secret Service가 필요합니다.
한글 표시에는 Noto Sans CJK(`fonts-noto-cjk`) 또는 나눔고딕 글꼴을 권장합니다.

앱에 코드 서명이 되어 있지 않아 처음 실행할 때 위와 같은 경고가 나옵니다.
