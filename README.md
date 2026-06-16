# Nexterm

Linux-first remote session manager на Rust — концептуальный аналог
MobaXterm / mRemoteNG. Цель проекта: единое окно с вкладками для
SSH/Telnet/RDP/VNC/SFTP и т.д., менеджер туннелей, мульти-ввод, встроенные
мини-серверы и безопасное хранение секретов.

> **Статус: ранний MVP.** Это инженерно-корректная отправная точка, а не
> готовый продукт. Готов каркас workspace, доменные модели, абстракции
> протоколов и хранения секретов, рабочий HTTP мини-сервер, планировщик,
> CLI-харнесс с тестами, **реальный SSH/SFTP-транспорт через `russh`** (флаг
> `ssh-russh`) и local-shell через PTY. GUI (Qt) идёт следующими итерациями.
> Подробности — в разделе «Дорожная карта».
>
> Раньше проект назывался `rust-remote-suite`; внутренние имена крейтов (`rrs-*`)
> и бинаря (`rrs`) пока сохранены — переименование в `nexterm` отложено в
> отдельный churn-only проход.

## Что уже работает
- Многокрейтовый Cargo workspace с чётким графом зависимостей.
- Доменные модели (профили, группы, сессии), конфиг (TOML), event bus.
- Хранение профилей в JSON-файле за трейтом `ProfileStore` (без секретов).
- Абстракция секретов: тип `Secret` (zero-on-drop, без `Display`/`Serialize`),
  трейт `CredentialStore`, in-memory backend по умолчанию и backend OS-keyring
  под флагом `keyring-os`.
- Трейтовая модель транспортов (`Connector`/`RemoteSession`/`SftpClient`) с
  рабочим mock-SSH, local-shell через PTY (флаг `local-pty`) и **реальным
  SSH/SFTP через `russh`** (флаг `ssh-russh`): PTY-shell, auth agent→key→
  password→keyboard-interactive, проверка `known_hosts`.
- Менеджер SSH-туннелей (local/remote/dynamic) с mock-драйвером и тестами,
  плюс **реальный драйвер local-forwarding (`ssh -L`) через `russh`** (флаг
  `ssh-russh`): `direct-tcpip`-канал на каждое входящее соединение,
  bidirectional-прокачка байт, корректный shutdown по `stop`/Ctrl-C. Remote
  (`-R`) и dynamic SOCKS (`-D`) пока возвращают `Unsupported`.
- **Single-hop jump-host (`ProxyJump`)** для **shell и SFTP**: подключение к
  target *через* gateway по `direct-tcpip` — реальная вторая SSH-сессия поверх
  канала, не `ssh target` внутри shell. Host-key и auth проверяются для обоих
  хостов независимо.
- **Оркестрация jump-host в `AppCore`**: обычный `AppCore::connect(profile)` и
  `AppCore::connect_sftp(profile)` сами резолвят gateway-профиль из `ProfileStore`
  и секреты обоих хопов из `CredentialStore` (транзиентно), если у SSH-профиля
  задан `jump_host`. `Connector` остаётся без доступа к хранилищам — будущий GUI
  подключается через jump одним вызовом, без ручной оркестрации.
- Рабочий HTTP-файловый мини-сервер (axum) и планировщик-мини-сервер.
- Логика, не зависящая от GUI: мульти-ввод с защитой от опасных команд, макросы
  с предупреждением о секретах, детектор конфликтов при правке удалённых файлов,
  модель подсветки вывода с защитой от порчи TUI (alt-screen).
- CLI-харнесс `rrs` и юнит-тесты по ключевым крейтам.

## Архитектура (кратко)
```
apps/cli      -> бинарь `rrs` (харнесс)
apps/qt       -> Qt-фронтенд (заготовка; см. apps/qt/README.md)
apps/gtk      -> GTK-фронтенд (заготовка)

crates/ui-common  -> AppCore (фасад) + мульти-ввод/макросы/конфликты/safety
crates/core       -> модели, конфиг, события, реестр сессий, ProfileStore
crates/credentials-> Secret + CredentialStore (memory / OS keyring)
crates/protocols  -> Connector/RemoteSession/SftpClient (+ SSH mock/russh)
crates/terminal   -> подсветка, alt-screen, PTY (feature `pty`)
crates/tunnels    -> модель и менеджер SSH-туннелей
crates/miniservers-> framework мини-серверов + HTTP + scheduler
crates/platform   -> пути/идентичность ОС (Linux-first)
xtask             -> запуск задач (build/test/fmt/lint)
```
Принцип: фронтенды держат `Arc<AppCore>` и ничего не знают о транспортах и
хранилищах. Новый протокол = новая реализация трейта, без правок UI.

## Требования
- Rust (stable). Код 2024-ready, но в скелете зафиксирован edition 2021 для
  предсказуемой первой сборки (см. комментарий в корневом `Cargo.toml`).
- Системные библиотеки только для отдельных feature-флагов и будущего GUI (ниже).

### Системные зависимости

**Arch Linux / KDE Plasma (целевая среда)**
```bash
sudo pacman -S --needed base-devel pkgconf openssl cmake
# OS-keyring в рантайме на KDE предоставляет KWallet (обычно уже есть).
# Вне KDE можно поставить gnome-keyring:
# sudo pacman -S --needed gnome-keyring
# Будущий Qt-фронтенд:
sudo pacman -S --needed qt6-base qt6-declarative
# Будущий GTK-фронтенд:
sudo pacman -S --needed gtk4
```

**Ubuntu / Debian**
```bash
sudo apt update
sudo apt install -y build-essential pkg-config libssl-dev cmake
# Будущий Qt: qt6-base-dev qt6-declarative-dev
# Будущий GTK: libgtk-4-dev
```

**Fedora**
```bash
sudo dnf install -y @development-tools pkgconf-pkg-config openssl-devel cmake
# Будущий Qt: qt6-qtbase-devel qt6-qtdeclarative-devel
# Будущий GTK: gtk4-devel
```

### Установка Rust
```bash
# Рекомендуется rustup:
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup default stable
# Либо дистрибутивный пакет (Arch): sudo pacman -S rust
```

## Сборка и запуск
```bash
cd rust-remote-suite

# Debug-сборка всего workspace (фичи по умолчанию: mock-SSH, in-memory секреты,
# HTTP-сервер). Тянет только широко используемые крейты.
cargo build

# Release:
cargo build --release

# Базовая проверка окружения:
cargo run -p rrs-cli -- check

# HTTP-файловый сервер (loopback по умолчанию):
cargo run -p rrs-cli -- serve-http --root . --port 8080

# Демо mock-SSH (без сети):
cargo run -p rrs-cli -- ssh-demo

# Локальный shell через PTY поверх AppCore (нужна фича local-pty):
cargo run -p rrs-cli --features local-pty -- local-shell --command "echo hi"

# Реальный SSH (нужна фича ssh-russh). Пароль — из env (dev-only, не печатается):
NEXTERM_SSH_PASSWORD='secret' \
cargo run -p rrs-cli --features ssh-russh -- ssh-connect \
  --host 127.0.0.1 --user test --command 'echo SSH_OK; uname -a'
# Аутентификация по ключу + строгая проверка known_hosts:
cargo run -p rrs-cli --features ssh-russh -- ssh-connect \
  --host 192.168.1.10 --user root --key ~/.ssh/id_ed25519

# Jump-host: shell на target ЧЕРЕЗ gateway (direct-tcpip; нужна фича ssh-russh).
# Пароли — из отдельных env-переменных (dev-only, не печатаются); ключи — на хоп.
NEXTERM_JUMP_PASSWORD='gw-pw' NEXTERM_TARGET_PASSWORD='t-pw' \
cargo run -p rrs-cli --features ssh-russh -- ssh-jump-connect \
  --jump-host 192.168.1.10 --jump-user jumpuser \
  --target-host 10.10.10.5 --target-user root \
  --command 'echo JUMP_OK; hostname'
# Или по ключам:
cargo run -p rrs-cli --features ssh-russh -- ssh-jump-connect \
  --jump-host gw --jump-user me --jump-key ~/.ssh/id_ed25519 \
  --target-host 10.10.10.5 --target-user root --target-key ~/.ssh/id_ed25519

# Local port-forward (ssh -L) до Ctrl-C (нужна фича ssh-russh):
NEXTERM_SSH_PASSWORD='secret' \
cargo run -p rrs-cli --features ssh-russh -- tunnel-local \
  --ssh-host 127.0.0.1 --ssh-user test \
  --bind 127.0.0.1:18080 --target 127.0.0.1:80
# Теперь подключения на 127.0.0.1:18080 форвардятся через SSH-хост на target.

# SFTP: листинг каталога (нужна фича ssh-russh). Пароль — из env (dev-only):
NEXTERM_SSH_PASSWORD='secret' \
cargo run -p rrs-cli --features ssh-russh -- sftp-ls \
  --host 127.0.0.1 --user test --path /etc
# По ключу:
cargo run -p rrs-cli --features ssh-russh -- sftp-ls \
  --host 192.168.1.10 --user root --key ~/.ssh/id_ed25519 --path /var/log

# SFTP ЧЕРЕЗ jump-host (direct-tcpip): листинг каталога на target через gateway.
NEXTERM_JUMP_PASSWORD='gw-pw' NEXTERM_TARGET_PASSWORD='t-pw' \
cargo run -p rrs-cli --features ssh-russh -- sftp-jump-ls \
  --jump-host 192.168.1.10 --jump-user jumpuser \
  --target-host 10.10.10.5 --target-user root --path /etc

# Подсветка строки вывода:
cargo run -p rrs-cli -- highlight "iface eth0 is up at 10.0.0.1 error"

# Проверка команды детектором опасных шаблонов (мульти-ввод):
cargo run -p rrs-cli -- danger-check "sudo rm -rf /"

# Профили (JSON-хранилище):
cargo run -p rrs-cli -- profiles add-ssh myhost 10.0.0.1 --user admin
cargo run -p rrs-cli -- profiles list

# Все тесты:
cargo test --workspace
```

### Feature-флаги
- `keyring-os` — backend OS-keyring (Secret Service / KWallet / GNOME Keyring).
  Пример: `cargo run -p rrs-cli --features keyring-os -- check`.
- `ssh-russh` — **реальный SSH/SFTP** через `russh` 0.61 + `russh-sftp` 2.3
  (крипто-бэкенд `ring`). Готово: PTY-shell как `RemoteSession`, SFTP
  (`RusshSftp`), аутентификация agent→key→password→keyboard-interactive,
  проверка `known_hosts` с учётом `strict_host_key_checking`, **single-hop
  jump-host (`direct-tcpip`)** и **реальный local-forwarding tunnel driver**.
  Внутри — единый переиспользуемый примитив `SshConnection` (connect / connect
  через jump / `open_shell` / `open_sftp` / `open_forward_stream`), без
  копипасты auth/known_hosts.
  - Auth methods реально готовы: **agent, public key, password,
    keyboard-interactive**. Strict mode: неизвестный host-key → отказ;
    non-strict → подключение + warning; изменённый ключ → всегда отказ.
  - **Jump-host** для **shell** (`ssh-jump-connect`) и **SFTP**
    (`sftp-jump-ls`): открывается `direct-tcpip` канал на gateway к
    `target:port`, поверх него поднимается отдельная SSH-сессия к target
    (host-key + auth проверяются для обоих хостов). Пользователь получает
    shell/SFTP на **target**. Пароли — из `NEXTERM_JUMP_PASSWORD` /
    `NEXTERM_TARGET_PASSWORD` (dev-only, не печатаются); ключи — `--jump-key` /
    `--target-key`. Ограничение: один gateway (цепочки длины > 1 — TODO).
  - **Оркестрация в `AppCore`**: `Connector` расширен методами
    `connect_shell_via_jump` / `connect_sftp` / `connect_sftp_via_jump`
    (default → `NotImplemented`), и `AppCore::connect`/`connect_sftp` сами
    резолвят gateway-профиль из `ProfileStore` + секреты обоих хопов из
    `CredentialStore`, когда у профиля задан `jump_host`. `ProfileStore` не
    протаскивается в `Connector`; ошибки явные (gateway not found / not SSH /
    chain > 1). Эта логика трейтовая и тестируется в default-сборке
    (`cargo test -p rrs-ui-common`, без `russh`).
  - **SFTP** (`sftp-ls`, `sftp-jump-ls`): `RusshSftp::connect` (direct) и
    `RusshSftp::connect_via_jump` поверх того же `SshConnection` —
    auth/known_hosts не дублируются.
  - **Tunnel driver** (`tunnel-local`): `RusshTunnelDriver` биндит локальный
    listener и форвардит каждое соединение через `direct-tcpip` на
    `target:port`. Поддержан **только local-forwarding (`-L`)**; remote (`-R`) и
    dynamic SOCKS (`-D`) → `TunnelError::Unsupported`. Драйвер живёт в
    `crates/tunnels` (фича `ssh-russh`, dep `rrs-protocols`); граф остаётся
    однонаправленным `tunnels → protocols → core`.
  - `ssh-connect`/`ssh-jump-connect`/`tunnel-local`/`sftp-ls`/`sftp-jump-ls`
    читают пароли из env (`--password-env` и аналоги) — это **временный
    dev-харнесс**, не финальный UX: в проде секрет лежит в OS-keyring и
    резолвится транзиентно.
  - Live-проверки `direct-tcpip` помечены `#[ignore]` и требуют sshd:
    `jump_host_roundtrip` и `sftp_jump_roundtrip` (`rrs-protocols`, env
    `NEXTERM_JUMP_TEST_*` + `NEXTERM_TARGET_TEST_*`, для SFTP ещё
    `NEXTERM_TARGET_TEST_SFTP_PATH`) и `local_tunnel_roundtrip` (`rrs-tunnels`,
    env `NEXTERM_SSH_TEST_*`); плюс `sftp_roundtrip` (direct).
  - Ещё не сделано: цепочки jump-host длины > 1; remote (`-R`) / dynamic SOCKS
    (`-D`) туннели; проброс SSH-агента.
- `pty` — локальный shell через PTY (`portable-pty`), в крейте `rrs-terminal`.
- `local-pty` (на `rrs-cli`/`rrs-protocols`) — local-shell как полноценный транспорт
  через `AppCore::connect` (`LocalShellConnector`/`LocalPtySession`); тянет `pty`.
  Пример: `cargo run -p rrs-cli --features local-pty -- local-shell`.

Запуск вспомогательных задач:
```bash
cargo run -p xtask -- build      # | build-release | test | fmt | lint | run-cli
```

## Безопасность секретов
- Профили и группы **никогда** не содержат паролей/ключей — только `CredentialRef`
  (UUID + нcaption-метка). Сам секрет лежит в OS-keyring (с `keyring-os`) или
  только в памяти (по умолчанию) и зануляется при удалении (`zeroize`).
- Тип `Secret` не реализует `Display`/`Serialize`, а его `Debug` печатает
  `Secret(***)`, чтобы секреты не утекали в логи/файлы.
- Блокирующие вызовы keyring выполняются на blocking-пуле, не на UI-потоке.

## Логи
Через `RUST_LOG` (по умолчанию `info`):
```bash
RUST_LOG=debug cargo run -p rrs-cli -- ssh-demo
RUST_LOG=rrs_miniservers=debug,info cargo run -p rrs-cli -- serve-http
```

## Решение проблем
- `cannot find -lssl` / ошибки линковки — поставьте OpenSSL dev и pkg-config
  (`openssl`/`libssl-dev`/`openssl-devel` + `pkgconf`/`pkg-config`).
- `pkg-config not found` — установите `pkgconf` (Arch) / `pkg-config`.
- keyring в рантайме ругается на отсутствие Secret Service — на KDE запущен
  KWallet, вне KDE поставьте `gnome-keyring`; нужна активная сессия D-Bus.
- сборка с `ssh-russh` требует C-компилятора и `cmake` (крипто-зависимости).
- если конкретная версия axum не примет `Router` в `axum::serve` — замените
  `app` на `app.into_make_service()` в `crates/miniservers/src/http.rs`
  (или закрепите `axum = "0.7"`, API для этого использования совпадает).
- будущий Qt-фронтенд требует Qt6 + CMake/qmake.

## Дорожная карта
- **v0.2:** реальный SSH+SFTP через `russh` ✓; реальный PTY ✓; single-hop
  jump-host для shell и SFTP ✓; оркестрация jump-host в `AppCore` ✓; реальный
  local-forwarding tunnel driver ✓; SQLite-хранилище с миграциями; Qt/QML-скелет
  (одно окно + вкладка поверх `AppCore`).
- **v0.3:** полноценная SGR-aware подсветка; вендорные пресеты (Cisco/MikroTik/
  …); **цепочки jump-host (длина > 1)**; **remote (`-R`) и dynamic SOCKS (`-D`)
  туннели**; проброс SSH-агента; больше мини-серверов (TFTP/FTP/SSH);
  GTK-фронтенд.
- **Долгосрочно:** RDP (IronRDP) и VNC; обёртки X-сервера (Xephyr/Xvfb/Xwayland,
  без переписывания Xorg); серверы NFS/Telnet/VNC; интеграция с systemd
  (user-services); поддержка Windows (backend Windows Credential Manager за тем
  же трейтом `CredentialStore`); проброс агента SSH.

## Лицензия
MIT OR Apache-2.0.
