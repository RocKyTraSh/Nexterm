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
  плюс **реальный драйвер через `russh`** (флаг `ssh-russh`) для **всех трёх
  видов форвардинга**: **local (`ssh -L`)**, **dynamic SOCKS5 (`ssh -D`)** и
  **remote (`ssh -R`)**. Bidirectional-прокачка байт, корректный shutdown по
  `stop`/Ctrl-C. `-L`/`-D` — `direct-tcpip`-канал на исходящее соединение; `-R` —
  серверный `tcpip-forward` + входящие `forwarded-tcpip` каналы обратно к
  клиенту. SOCKS5: только NO AUTH + CONNECT (без UDP/BIND/SOCKS4).
- **Multi-hop jump-host (`ProxyJump`) chains** для **shell и SFTP**: подключение
  к target через цепочку gateway'ев `gateway1 → gateway2 → … → target` по
  вложенным `direct-tcpip` каналам — реальная SSH-сессия на каждом хопе, не
  `ssh target` внутри shell. Host-key и auth проверяются для каждого хопа
  независимо. Глубина ограничена `MAX_JUMP_CHAIN = 8`; циклы и слишком длинные
  цепочки явно отлавливаются. Single-hop — частный случай (одна gateway).
- **Оркестрация jump-host в `AppCore`**: обычный `AppCore::connect(profile)` и
  `AppCore::connect_sftp(profile)` сами разворачивают цепочку gateway-профилей из
  `ProfileStore` (идя по `jump_host`) в порядок подключения и резолвят секреты
  каждого хопа из `CredentialStore` (транзиентно). `Connector` остаётся без
  доступа к хранилищам — будущий GUI подключается через цепочку одним вызовом,
  без ручной оркестрации.
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

# Зашифрованный private key: passphrase — из ОТДЕЛЬНОЙ env (не из password!):
NEXTERM_SSH_KEY_PASSPHRASE='secret' \
cargo run -p rrs-cli --features ssh-russh -- ssh-connect \
  --host SERVER --user USER --key ~/.ssh/id_ed25519_encrypted \
  --key-passphrase-env NEXTERM_SSH_KEY_PASSPHRASE \
  --command 'echo KEY_OK; whoami'
# То же для SFTP:
NEXTERM_SSH_KEY_PASSPHRASE='secret' \
cargo run -p rrs-cli --features ssh-russh -- sftp-ls \
  --host SERVER --user USER --key ~/.ssh/id_ed25519_encrypted \
  --key-passphrase-env NEXTERM_SSH_KEY_PASSPHRASE --path /etc
# Через chain (общий ключ + общая passphrase на все хопы, dev-only):
NEXTERM_JUMP1_PASSWORD='' NEXTERM_TARGET_PASSWORD='' NEXTERM_SSH_KEY_PASSPHRASE='secret' \
cargo run -p rrs-cli --features ssh-russh -- ssh-chain-connect \
  --jump gw1:user1 --target target:user2 --key ~/.ssh/id_ed25519_encrypted \
  --key-passphrase-env NEXTERM_SSH_KEY_PASSPHRASE --command 'echo CHAIN_KEY_OK'

# SSH agent forwarding (выключено по умолчанию; нужен запущенный агент):
#   eval "$(ssh-agent)"; ssh-add ~/.ssh/id_ed25519
cargo run -p rrs-cli --features ssh-russh -- ssh-connect \
  --host 192.168.1.10 --user root --key ~/.ssh/id_ed25519 \
  --agent-forwarding --command 'echo "$SSH_AUTH_SOCK"; ssh-add -l'
# Если ssh-add -l на target показывает ключи локального агента — forwarding работает.

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

# Dynamic SOCKS5 proxy (ssh -D) до Ctrl-C (нужна фича ssh-russh):
NEXTERM_SSH_PASSWORD='secret' \
cargo run -p rrs-cli --features ssh-russh -- tunnel-socks \
  --ssh-host 127.0.0.1 --ssh-user test --bind 127.0.0.1:1080
# Используйте --socks5-hostname, чтобы DNS-резолв шёл через SOCKS, не локально:
curl --socks5-hostname 127.0.0.1:1080 https://ifconfig.me
curl --socks5-hostname 127.0.0.1:1080 https://example.com

# Remote port-forward (ssh -R) до Ctrl-C (нужна фича ssh-russh):
#   1. На локальной машине поднимите target-сервис, например:
#        python -m http.server 8080 --bind 127.0.0.1
#   2. Запросите у SSH-сервера слушать 127.0.0.1:18080 и форвардить к target:
NEXTERM_SSH_PASSWORD='secret' \
cargo run -p rrs-cli --features ssh-russh -- tunnel-remote \
  --ssh-host SERVER --ssh-user USER \
  --remote-bind 127.0.0.1:18080 --local-target 127.0.0.1:8080
#   3. НА SSH-СЕРВЕРЕ: curl http://127.0.0.1:18080  → ответит локальный python.
# Нужен server-side AllowTcpForwarding; bind на 0.0.0.0 требует GatewayPorts.

# Remote port-forward (ssh -R) ЧЕРЕЗ jump-chain (нужна фича ssh-russh):
#   bind происходит на FINAL TARGET; forwarded-tcpip каналы идут обратно через chain.
#   1. На локальной машине: python -m http.server 8080 --bind 127.0.0.1
#   2. Подключитесь к target через gateway'и и запросите -R на target:
NEXTERM_JUMP1_PASSWORD='pw1' NEXTERM_TARGET_PASSWORD='pwt' \
cargo run -p rrs-cli --features ssh-russh -- tunnel-remote-chain \
  --jump gw:user --target target:user \
  --remote-bind 127.0.0.1:18080 --local-target 127.0.0.1:8080
#   Несколько gateway'ев — повторяя --jump в порядке подключения; общий --key.
#   3. НА TARGET SSH-СЕРВЕРЕ: curl http://127.0.0.1:18080  → ответит локальный python.
# Нужен AllowTcpForwarding на target; non-loopback bind — GatewayPorts; gateways
# должны разрешать direct-tcpip к следующему hop. Bind НЕ на gateway, а на target.

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

# Multi-hop CHAIN: shell на target через gw1 -> gw2 (нужна фича ssh-russh).
# --jump повторяется в порядке подключения; пароли — по индексу из env.
NEXTERM_JUMP1_PASSWORD='pw1' NEXTERM_JUMP2_PASSWORD='pw2' NEXTERM_TARGET_PASSWORD='pwt' \
cargo run -p rrs-cli --features ssh-russh -- ssh-chain-connect \
  --jump gw1:user1 --jump gw2:user2 --target target:user3 \
  --command 'echo CHAIN_OK; hostname'
# Тот же chain по общему ключу на все хопы:
cargo run -p rrs-cli --features ssh-russh -- ssh-chain-connect \
  --jump gw1:me --jump gw2:me --target t:root --key ~/.ssh/id_ed25519
# Agent forwarding работает и через chain — пробрасывается только на target shell:
cargo run -p rrs-cli --features ssh-russh -- ssh-chain-connect \
  --jump gw1:me --jump gw2:me --target t:root --key ~/.ssh/id_ed25519 \
  --agent-forwarding --command 'ssh-add -l'

# SFTP через цепочку jump-host:
NEXTERM_JUMP1_PASSWORD='pw1' NEXTERM_JUMP2_PASSWORD='pw2' NEXTERM_TARGET_PASSWORD='pwt' \
cargo run -p rrs-cli --features ssh-russh -- sftp-chain-ls \
  --jump gw1:user1 --jump gw2:user2 --target target:user3 --path /etc

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
  проверка `known_hosts` с учётом `strict_host_key_checking`, **multi-hop
  jump-host chains (`direct-tcpip`)**, **реальный tunnel driver (все три вида:
  local `-L` + dynamic SOCKS5 `-D` + remote `-R`)** и **SSH agent forwarding**.
  Внутри — единый переиспользуемый примитив `SshConnection` (connect / connect
  через цепочку jump / `open_shell` / `open_sftp` / `open_forward_stream` /
  `request_remote_forward`), без копипасты auth/known_hosts.
  - Auth methods реально готовы: **agent, public key, password,
    keyboard-interactive**. Strict mode: неизвестный host-key → отказ;
    non-strict → подключение + warning; изменённый ключ → всегда отказ.
  - **Раздельные credentials** (password vs key-passphrase). У SSH-профиля
    независимо: **password** (ref `ConnectionProfile.credential`), **private key
    path** (`SshSettings.private_key_path`), **private key passphrase** (ref
    `SshSettings.key_passphrase` — отдельный `CredentialRef`, добавлен в этой
    итерации), **agent** и **keyboard-interactive**. Резолв (`AppCore`) тянет
    password и passphrase в **разные** поля `ResolvedCredentials` и НЕ смешивает
    их: `password`/`keyboard-interactive` используют только password,
    `load_secret_key` — только passphrase. Dangling ref (есть ссылка, нет секрета
    в сторе) → понятная ошибка. **Passphrase НЕ хранится в профиле** — только
    `CredentialRef` (UUID + метка); сам секрет в OS-keyring (или памяти) и
    резолвится транзиентно. CLI dev-harness берёт passphrase из env
    (`--key-passphrase-env`); финальный GUI будет сохранять её в keyring через
    `CredentialStore`. (Замечание: копия passphrase в `String` для API russh
    `load_secret_key` транзиентна, не логируется, но рвёт zeroize для копии — как
    и password-путь.)
  - **Agent forwarding** (флаг `--agent-forwarding` у `ssh-connect`,
    `ssh-jump-connect`, `ssh-chain-connect`; поле `SshSettings.agent_forwarding`,
    **по умолчанию выключено**). Когда включено и `$SSH_AUTH_SOCK` задан: на
    session-канале запрашивается `auth-agent-req@openssh.com`; входящие
    `auth-agent@openssh.com` каналы handler **прозрачно проксирует** на локальный
    агент-сокет (`copy_bidirectional` к `UnixStream`). Agent-протокол **не
    парсится и не логируется**; приватный ключ не передаётся (агент только
    подписывает). Если включено, но `$SSH_AUTH_SOCK` не задан — **fail-closed**:
    понятная ошибка (`agent forwarding requested but no local SSH agent`), а не
    тихое продолжение. Работает для **shell** (direct и через jump-chain —
    forwarding только на target shell, не на gateways). **SFTP и tunnels не
    требуют** agent forwarding и его не запрашивают.
    Ручная проверка: `--command 'echo "$SSH_AUTH_SOCK"; ssh-add -l'` — если на
    target виден сокет и ключи локального агента, forwarding работает. **Риск
    безопасности**: пока соединение активно, доверенный сервер может попросить
    forwarded-агент подписать произвольные запросы — включайте только для
    доверенных серверов.
  - **Jump-host chains** для **shell** (`ssh-jump-connect`, `ssh-chain-connect`)
    и **SFTP** (`sftp-jump-ls`, `sftp-chain-ls`): на каждом хопе открывается
    `direct-tcpip` канал к следующему хопу, поверх него поднимается отдельная
    SSH-сессия (host-key + auth для каждого хопа). Пользователь получает
    shell/SFTP на **target**. Порядок `gateway1 → … → gatewayN → target`; лимит
    `MAX_JUMP_CHAIN = 8`; циклы/слишком длинные цепочки → явная ошибка.
    Single-hop команды: пароли из `NEXTERM_JUMP_PASSWORD` /
    `NEXTERM_TARGET_PASSWORD`, ключи `--jump-key`/`--target-key`. Chain-команды:
    `--jump HOST:USER` повторяется в порядке подключения, пароли из
    `NEXTERM_JUMP<i>_PASSWORD` (1-based) и `NEXTERM_TARGET_PASSWORD`, общий
    `--key` на все хопы (dev-only, не печатаются).
  - **Оркестрация в `AppCore`**: `Connector` расширен методами
    `connect_shell_via_jump[_chain]` / `connect_sftp[_via_jump[_chain]]`
    (default → `NotImplemented`), и `AppCore::connect`/`connect_sftp` сами
    разворачивают цепочку gateway-профилей из `ProfileStore` (по `jump_host`) в
    порядок подключения + резолвят секреты каждого хопа из `CredentialStore`,
    когда у профиля задан `jump_host`. `ProfileStore` не протаскивается в
    `Connector`; ошибки явные (gateway not found / not SSH / cycle detected /
    chain too deep). Логика трейтовая и тестируется в default-сборке
    (`cargo test -p rrs-ui-common`, без `russh`).
  - **SFTP** (`sftp-ls`, `sftp-jump-ls`, `sftp-chain-ls`): `RusshSftp::connect`
    (direct) и `RusshSftp::connect_via_jump` / `connect_via_jump_chain` поверх
    того же `SshConnection` — auth/known_hosts не дублируются.
  - **Tunnel driver** (`RusshTunnelDriver`). Отличие трёх видов:
    - **Local (`-L`)** (`tunnel-local`): локальный listener → `direct-tcpip` →
      фиксированный remote `target:port` из спека.
    - **Dynamic SOCKS5 (`-D`)** (`tunnel-socks`): локальный SOCKS-listener →
      `direct-tcpip` → target из CONNECT-запроса. Поддержано: SOCKS5,
      `NO AUTH` (`0x00`), `CONNECT` (`0x01`), адреса IPv4/domain/IPv6. НЕ
      поддержано: SOCKS4/4a, username/password auth, `UDP ASSOCIATE`, `BIND`.
      SOCKS-парсер (`crates/tunnels/src/socks5.rs`) — чистые функции под
      юнит-тестами, в default-сборке (без `russh`). SOCKS success-reply шлётся
      только после успешного открытия `direct-tcpip`; на ошибки — корректный
      failure-reply.
    - **Remote (`-R`)** — direct (`tunnel-remote`) **и через jump-chain**
      (`tunnel-remote-chain`): через `tcpip-forward` target-сервер слушает
      `--remote-bind` и открывает обратно к клиенту `forwarded-tcpip` каналы; на
      каждый канал Nexterm подключается к `--local-target` (на машине клиента) и
      прокачивает байты. Входящие каналы доставляются через handler соединения
      (`ForwardedConnection`). **Для chain `tcpip-forward` запрашивается на FINAL
      TARGET** (последний hop), `forwarded-tcpip` идут обратно через цепочку —
      `RusshTunnelDriver::connect_via_jump_chain` строит target-соединение через
      gateway'и и драйвит `-R` поверх него. Remote bind port `0` поддержан
      (сервер выбирает порт, он логируется). На `stop`/Ctrl-C —
      `cancel-tcpip-forward`. Одно SSH-соединение держит **один** активный `-R`
      (receiver берётся один раз). Ограничения: нужен `AllowTcpForwarding` на
      target; bind на non-loopback требует `GatewayPorts`; привилегированный порт
      может требовать root; gateways должны разрешать `direct-tcpip` к следующему
      hop. Несколько `-R` на одно соединение пока не поддержано.
    Драйвер живёт в `crates/tunnels` (фича `ssh-russh`, dep `rrs-protocols`);
    граф остаётся однонаправленным `tunnels → protocols → core`.
  - Все dev CLI-команды (`ssh-connect`, `ssh-jump-connect`, `ssh-chain-connect`,
    `tunnel-local`, `tunnel-socks`, `tunnel-remote`, `tunnel-remote-chain`,
    `sftp-ls`, `sftp-jump-ls`, `sftp-chain-ls`) читают пароли из env — это
    **временный dev-харнесс**, не финальный UX: в проде секрет лежит в OS-keyring
    и резолвится транзиентно.
  - Live-проверки помечены `#[ignore]` и требуют sshd:
    `jump_host_roundtrip`, `sftp_jump_roundtrip`, `jump_chain_roundtrip`,
    `sftp_jump_chain_roundtrip` (`rrs-protocols`; single-hop — env
    `NEXTERM_JUMP_TEST_*` + `NEXTERM_TARGET_TEST_*`; chain — `NEXTERM_CHAIN_JUMP1_*`,
    `NEXTERM_CHAIN_JUMP2_*`, `NEXTERM_CHAIN_TARGET_*` + `NEXTERM_CHAIN_TARGET_SFTP_PATH`),
    `local_tunnel_roundtrip`, `dynamic_socks_roundtrip`, `remote_tunnel_roundtrip`
    и `remote_tunnel_chain_roundtrip` (`rrs-tunnels`, env `NEXTERM_SSH_TEST_*` или
    `NEXTERM_CHAIN_*`; для `-R` ещё `NEXTERM_REMOTE_TEST_BIND` /
    `NEXTERM_REMOTE_CHAIN_TEST_BIND`; для curl-проверки SOCKS —
    `NEXTERM_SOCKS_TEST_URL`), `agent_forwarding_roundtrip` (`rrs-protocols`, env
    `NEXTERM_SSH_TEST_*` + запущенный `SSH_AUTH_SOCK`),
    `encrypted_key_passphrase_roundtrip` (`rrs-protocols`, env
    `NEXTERM_SSH_TEST_*` + `NEXTERM_SSH_TEST_ENCRYPTED_KEY` +
    `NEXTERM_SSH_TEST_KEY_PASSPHRASE`), плюс `sftp_roundtrip` (direct).
  - Ещё не сделано: несколько `-R` на одно соединение; key-passphrase в
    `profiles add-ssh` (CLI пока через env; GUI будет писать в keyring через
    `CredentialStore`).
- `pty` — локальный shell через PTY (`portable-pty`), в крейте `rrs-terminal`.
- `local-pty` (на `rrs-cli`/`rrs-protocols`) — local-shell как полноценный транспорт
  через `AppCore::connect` (`LocalShellConnector`/`LocalPtySession`); тянет `pty`.
  Пример: `cargo run -p rrs-cli --features local-pty -- local-shell`.

Запуск вспомогательных задач:
```bash
cargo run -p xtask -- build      # | build-release | test | fmt | lint | run-cli
```

## Безопасность секретов
- Профили и группы **никогда** не содержат паролей/ключей/passphrase — только
  `CredentialRef` (UUID + несекретная метка). Сам секрет лежит в OS-keyring (с
  `keyring-os`) или только в памяти (по умолчанию) и зануляется при удалении
  (`zeroize`).
- **Password и private-key passphrase — разные секреты с разными `CredentialRef`**:
  password — `ConnectionProfile.credential`; passphrase — `SshSettings.key_passphrase`.
  `AppCore` резолвит их в независимые поля `ResolvedCredentials` (`password` /
  `key_passphrase`) и не смешивает; encrypted key расшифровывается только
  passphrase, password идёт только в password/keyboard-interactive auth.
- Тип `Secret` не реализует `Display`/`Serialize`, а его `Debug` печатает
  `Secret(***)`; `ResolvedCredentials::Debug` редактирует оба секрета — чтобы
  ничего не утекало в логи/файлы.
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
- **v0.2:** реальный SSH+SFTP через `russh` ✓; реальный PTY ✓; multi-hop
  jump-host chains для shell и SFTP ✓; оркестрация jump-host chains в `AppCore`
  ✓; tunnel driver — local (`-L`) ✓, dynamic SOCKS5 (`-D`) ✓ и remote (`-R`) ✓;
  agent forwarding ✓; SQLite-хранилище с миграциями; Qt/QML-скелет (одно окно +
  вкладка поверх `AppCore`).
- **v0.3:** полноценная SGR-aware подсветка; вендорные пресеты (Cisco/MikroTik/
  …); tunnel-менеджмент в GUI; больше мини-серверов (TFTP/FTP/SSH); GTK-фронтенд.
- **Долгосрочно:** RDP (IronRDP) и VNC; обёртки X-сервера (Xephyr/Xvfb/Xwayland,
  без переписывания Xorg); серверы NFS/Telnet/VNC; интеграция с systemd
  (user-services); поддержка Windows (backend Windows Credential Manager за тем
  же трейтом `CredentialStore`).

## Лицензия
MIT OR Apache-2.0.
