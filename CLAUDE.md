# CLAUDE.md — Nexterm

Передаточный бриф для Claude Code. Проза по-русски, команды/пути/имена типов — как в коде (English). Прочитай целиком перед первой правкой.

> **Имя проекта — Nexterm** (раньше `rust-remote-suite`). Бренд в документации обновлён; внутренние имена крейтов (`rrs-*`), бинаря (`rrs`) и module paths пока сохранены — массовый rename в `nexterm`/`nexterm-*` отложен в отдельный churn-only проход (TODO в `apps/cli/src/main.rs`).

## Цель проекта

Nexterm — Linux-first remote session manager на Rust, концептуальный аналог **MobaXterm / mRemoteNG**. Целевая среда: Arch Linux / KDE Plasma. Финальное видение: одно окно с вкладками для SSH/Telnet/RDP/VNC/SFTP, менеджер туннелей, мульти-ввод, встроенные мини-серверы, безопасное хранение секретов, Qt-фронтенд (приоритет) и GTK-фронтенд.

**Сейчас это ранний MVP-скелет**, а не продукт. Задача каркаса — позволять добавлять протоколы и фичи по одной, без переписывания. Не пытайся реализовать «весь MobaXterm» разом.

## Команды

```bash
# Сборка/тесты (фичи по умолчанию: mock-SSH, in-memory секреты, HTTP-сервер)
cargo build
cargo build --release
cargo test --workspace
cargo fmt --all
cargo clippy --workspace --all-targets

# CLI-харнесс (бинарь `rrs`, пакет `rrs-cli`)
cargo run -p rrs-cli -- check
cargo run -p rrs-cli -- ssh-demo
cargo run -p rrs-cli --features local-pty -- local-shell --command "echo hi"  # PTY-сессия через AppCore
# Реальный SSH (фича ssh-russh); пароль из env (dev-only, не печатается):
NEXTERM_SSH_PASSWORD='pw' cargo run -p rrs-cli --features ssh-russh -- ssh-connect --host 127.0.0.1 --user test --command 'echo SSH_OK'
cargo run -p rrs-cli --features ssh-russh -- ssh-connect --host h --user root --key ~/.ssh/id_ed25519  # auth по ключу
cargo run -p rrs-cli -- highlight "iface eth0 is up at 10.0.0.1 error"
cargo run -p rrs-cli -- danger-check "sudo rm -rf /"
cargo run -p rrs-cli -- serve-http --root . --port 8080
cargo run -p rrs-cli -- profiles add-ssh myhost 10.0.0.1 --user admin
cargo run -p rrs-cli -- profiles list

# Раннер задач
cargo run -p xtask -- build      # | build-release | test | fmt | lint | run-cli

# Feature-флаги (по умолчанию ВЫКЛ)
cargo run -p rrs-cli --features keyring-os -- check   # OS-keyring backend
cargo build -p rrs-cli --features ssh-russh           # реальный SSH/SFTP (russh, бэкенд ring)
# ssh-russh — реальный SSH/SFTP; local-pty — локальный shell через AppCore; pty — PTY-бэкенд
# Live SFTP-тест (нужен sshd): NEXTERM_SSH_TEST_{HOST,USER,KEY}=… cargo test -p rrs-protocols --features ssh-russh -- --ignored sftp_roundtrip
```

Системные зависимости и troubleshooting — в `README.md`.

## Архитектура

UI-agnostic ядро + трейтовые границы. Фронтенды держат `Arc<AppCore>` и не знают о транспортах/хранилищах. Граф зависимостей строго однонаправленный:

```
apps/{cli,qt,gtk} ──▶ crates/ui-common ──▶ core, credentials, protocols, tunnels, miniservers
protocols ──▶ core + credentials      tunnels ──▶ core      core ──▶ platform
```

| Крейт | Ответственность |
|---|---|
| `crates/core` | модели, конфиг (TOML), event bus, реестр сессий, `ProfileStore` |
| `crates/credentials` | `Secret` (zeroize) + `CredentialStore` (memory / OS keyring) |
| `crates/protocols` | `Connector` / `RemoteSession` / `SftpClient` (+ SSH mock и russh-каркас) |
| `crates/terminal` | подсветка, детект alt-screen, PTY (feature `pty`) |
| `crates/tunnels` | модель и менеджер SSH-туннелей (+ mock-драйвер; real local-forward `RusshTunnelDriver` за `ssh-russh`) |
| `crates/miniservers` | framework мини-серверов + рабочий HTTP + scheduler |
| `crates/platform` | пути/идентичность ОС (Linux-first) |
| `crates/ui-common` | `AppCore` (фасад) + multiexec / macros / conflict / safety |
| `apps/cli` | бинарь `rrs` — харнесс без GUI |
| `apps/{qt,gtk}` | заготовки фронтендов |
| `xtask` | раннер задач |

## Инварианты — НЕ нарушать

1. **Секреты не хранятся в профилях/группах.** Только `CredentialRef` (UUID + несекретная метка). Сам секрет — в OS-keyring (`keyring-os`) или в памяти; резолвится **транзиентно** в момент connect через `ResolvedCredentials` и нигде не сохраняется/не логируется.
2. **`Secret` нельзя печатать/сериализовать.** Нет `Display`/`Serialize`; `Debug` выдаёт `Secret(***)`. Не добавляй обходов. Не пиши секреты в `tracing`.
3. **Новый протокол = новая реализация `Connector`/`RemoteSession` (+ `SftpClient`)**, без правок UI. Не протаскивай протокол-специфику в `ui-common`/фронтенды.
4. **Хранилище — только за трейтом `ProfileStore`.** В хранилище никаких секретов.
5. **Никакого блокирующего I/O на async/UI-потоке.** keyring → `spawn_blocking`; PTY → отдельный поток + канал.
6. **Дефолтная сборка должна оставаться «чистой»** (только широко известные крейты). Всё рискованное — за feature-флагами, ВЫКЛ по умолчанию.
7. **Подсветка подавляется в alt-screen** (`AltScreenTracker`). Не ломай ncurses/top/htop/vim. Подсвечивать только plain-text строки.
8. **Без лишних `unsafe`/`unwrap`/`expect`.** Ошибки — `thiserror` (либы) / `anyhow` (приложения).

## Принятые решения — НЕ переоткрывать без причины

- **edition = "2021"** в `Cargo.toml` (workspace). Код 2024-чистый; бамп до `2024` — одна строка, отложен до первого зелёного билда (приоритет — предсказуемая сборка).
- **Qt-подход: Rust core + QML через `cxx-qt`.** Альтернативы (тонкий C++/Qt Widgets + QTermWidget через `cxx`; демон + IPC; чистый Rust egui/iced/slint) разобраны и отклонены в `apps/qt/README.md`.
- **`russh` 0.61 + `russh-sftp` 2.3 — за фичей `ssh-russh`** (крипто-бэкенд `ring`, без nasm; `default-features = false` + `flate2`/`rsa`). `RusshConnector`/`RusshSftp` реализованы (shell + SFTP, auth, known_hosts). **Multi-hop jump-host chains реализованы для shell И SFTP** через вложенные `direct-tcpip` (`SshConnection::connect_via_jump_chain`; single-hop `connect_via_jump_host` — тонкая обёртка). Host-key проверка через встроенный `russh::keys::check_known_hosts`; политика strict/non-strict — чистые функции `decide_host_key`/`plan_auth`/`validate_jump_chain_endpoints` (юнит-тесты). Лимит `MAX_JUMP_CHAIN = 8`.
- **`Connector` trait расширен** default-методами `connect_shell_via_jump[_chain]` / `connect_sftp` / `connect_sftp_via_jump[_chain]` (default → `NotImplemented`). Это позволяет `AppCore` оркестрировать jump-chains и SFTP **только через trait-объект**, не зная про russh и не протаскивая `ProfileStore` в `Connector`. `RusshConnector` переопределяет все; single-hop методы делегируют в chain-методы. `JumpHop<'a>` (профиль + creds) — единица цепочки, передаётся как `&[JumpHop]`.
- **Единый SSH-примитив `SshConnection`** (в `russh_impl.rs`): инкапсулирует connect (TCP) / connect через цепочку (`hop_to` → `connect_over_stream` поверх `direct-tcpip`-канала) / host-key / auth и отдаёт `open_shell` / `open_sftp` / `open_forward_stream`. Shell/SFTP/chain/tunnels используют один путь — без копипасты auth/known_hosts. `establish()` удалён (заменён на `SshConnection::connect`).
- **Real `TunnelDriver` (`RusshTunnelDriver`) живёт в `crates/tunnels` за фичей `ssh-russh`** (optional dep `rrs-protocols`). Граф остаётся однонаправленным: `tunnels → protocols → core` (protocols НЕ зависит от tunnels). Реализованы **все три вида**: **local `-L`**, **dynamic SOCKS5 `-D`** (через `direct-tcpip`) и **remote `-R`** (через `tcpip-forward` + входящие `forwarded-tcpip`). Диспетчер режима — чистая функция `forward_mode_for(spec)` (Local/Dynamic → local listener; Remote → server bind). Альтернатива (драйвер в protocols, реализующий трейт tunnels) отклонена — она развернула бы граф `protocols → tunnels`.
- **Remote forwarding (`-R`) — доставка входящих каналов через handler.** `forwarded-tcpip` каналы приходят в `ClientHandler::server_channel_open_forwarded_tcpip` (в `russh_impl.rs`, protocols), а НЕ через `Handle`. Поэтому `ClientHandler` держит `mpsc::UnboundedSender<ForwardedConnection>`, а `SshConnection` — receiver (за `take_forwarded_connections`). `SshConnection::request_remote_forward`/`cancel_remote_forward` оборачивают `Handle::tcpip_forward`/`cancel_tcpip_forward`. Драйвер берёт receiver один раз → **один активный `-R` на SSH-соединение** (документировано). `ForwardedConnection` (метаданные + `DirectTcpipStream`) экспортируется из protocols.
- **`-R` через jump-chain — без нового механизма.** `SshConnection::connect_via_jump_chain` (каждый hop через `hop_to`→`connect_over_stream`) уже отдаёт **target** connection со своим `forwarded_rx`/handler, так что `request_remote_forward` запрашивает `tcpip-forward` НА TARGET (последний hop), а `forwarded-tcpip` идут обратно через цепочку. На уровне tunnels добавлен лишь конструктор `RusshTunnelDriver::connect_via_jump_chain(gateways, target, creds)` (обёртка `from_connection`). direct `-R`/`-L`/`-D` не тронуты; граф `tunnels → protocols → core` не меняется.
- **Agent forwarding — через handler-callback + проксирование к `$SSH_AUTH_SOCK`.** Когда `SshSettings.agent_forwarding` (по умолчанию `false`), `SshConnection::open_shell` запрашивает `channel.agent_forward(true)`; входящие `auth-agent@openssh.com` каналы приходят в `ClientHandler::server_channel_open_agent_forward` и прозрачно проксируются на локальный агент-сокет (`UnixStream` + `copy_bidirectional`). Решение «запрашивать или нет» — чистая `plan_agent_forwarding(enabled, has_sock)` (юнит-тест): disabled→skip, enabled+sock→request, enabled+no-sock→**fail-closed** (`ProtocolError::Agent`). Agent-протокол НИКОГДА не парсится и не логируется. Handler сервисит канал только если сокет был захвачен при connect (т.е. forwarding включён) — иначе незапрошенный agent-канал закрывается. Только для **shell** (SFTP/tunnels не запрашивают); через jump-chain — только на target shell (gateways не открывают shell). Unix-only (агент-сокет).
- **SOCKS5-парсер `crates/tunnels/src/socks5.rs` — в default-сборке** (без `russh`): чистые `select_method`/`parse_request`/`encode_reply` + async `negotiate_method`/`read_request`/`write_reply` (bounded `read_exact`). Только SOCKS5 / NO AUTH / CONNECT / IPv4·domain·IPv6; без UDP/BIND/SOCKS4. Это держит парсер тестируемым и переиспользуемым; реальный форвардинг по нему — за `ssh-russh` в `russh_driver.rs`.
- **`chrono` не используем** — `std::time::SystemTime`.
- **MVP-хранилище — JSON-файл** (`FileProfileStore`, атомарная запись temp+rename). SQLite с миграциями — v0.2, за тем же трейтом.
- **HTTP мини-сервер — `axum` 0.8 + `tower-http` ServeDir.** Если версия не примет голый `Router` в `axum::serve` → `app.into_make_service()` (помечено в `http.rs`) или закрепить `axum = "0.7"`.
- **keyring 3.x** с фичами `sync-secret-service` + `crypto-rust` (pure-Rust крипто, без OpenSSL).

## Важные файлы

- `crates/ui-common/src/app.rs` — **`AppCore`**, точка входа для фронтендов (`connect`, `connect_sftp`, `resolve_credentials`, `set_profile_secret`). **Jump-chain-оркестрация**: `connect`/`connect_sftp` при `jump_host.is_some()` разворачивают цепочку gateway-профилей из `ProfileStore` в порядок подключения + резолвят секреты каждого хопа и зовут `Connector::connect_*_via_jump_chain` (`resolve_jump_chain`: обход `jump_host`, cycle/depth-проверки). Pure-тесты (spy-`Connector` + in-memory `ProfileStore`, direct/1/2/3-hop/cycle/depth) — в default-сборке, без `russh`.
- `crates/core/src/model/profile.rs` — доменная модель, `CredentialRef`, `ProtocolSettings` (tagged enum, расширяется новыми вариантами).
- `crates/credentials/src/{secret,backend,memory,keyring_os}.rs` — безопасность секретов.
- `crates/protocols/src/traits.rs` — `Connector` / `RemoteSession` / `SftpClient` / `ResolvedCredentials`.
- `crates/protocols/src/ssh/{mock,russh_impl}.rs` — SSH: рабочий mock + **реальный russh-транспорт** (`RusshConnector`/`RusshSession`/`RusshSftp`, auth-план, host-key политика, `plan_agent_forwarding`). **`SshConnection`** — переиспользуемый примитив (connect / `connect_via_jump_chain` / `hop_to` / `open_shell` [+agent forwarding] /`open_sftp`/`open_forward_stream` / `request_remote_forward`/`cancel_remote_forward`/`take_forwarded_connections`); `DirectTcpipStream` — тип forward-стрима; `ForwardedConnection` — входящий `-R`-канал; `ClientHandler` маршрутизирует `forwarded-tcpip` И `auth-agent@openssh.com` (проксирование к `$SSH_AUTH_SOCK`). `JumpHop`/`MAX_JUMP_CHAIN` — в `crates/protocols/src/traits.rs` (не feature-gated, доступны `ui-common`).
- `crates/tunnels/src/{manager,russh_driver,socks5}.rs` — `TunnelManager` + трейт `TunnelDriver` + mock; **`RusshTunnelDriver`** (фича `ssh-russh`) — `-L`/`-D` (local listener → `open_forward_stream`) и `-R` (`forwarded_loop` поверх `ForwardedConnection`), диспетчер `forward_mode_for`; конструкторы `connect`/`connect_via_jump_chain`/`from_connection` (последний — для `-R` через chain); `socks5` — серверный SOCKS5-парсер (default-сборка). Pure-валидация спеков (`bind_endpoint`/`local_forward_target`/`remote_forward_endpoints`, `new_local`/`new_dynamic`/`new_remote`) под юнит-тестами.
- `crates/protocols/src/local.rs` — local-shell транспорт (фича `local-pty`): `LocalShellConnector` + `LocalPtySession` поверх `rrs_terminal::pty::LocalPty`.
- `crates/terminal/src/{altscreen,highlight,pty}.rs` — терминальная логика.
- `crates/miniservers/src/{service,http,scheduler}.rs` — framework + HTTP + scheduler.
- `apps/cli/src/main.rs` — харнесс для ручной проверки ядра.
- `apps/qt/README.md` — решение по GUI (decision record).

## Текущий прогресс

**Готово (с тестами по ключевым крейтам):** workspace и граф зависимостей; модели/конфиг/события/реестр сессий; `ProfileStore` (JSON); `Secret` + `CredentialStore` (memory + `keyring-os`); трейты протоколов + SSH mock; **реальный SSH/SFTP через `russh` (фича `ssh-russh`): PTY-shell `RusshSession`, `RusshSftp`, auth agent→key→password→keyboard-interactive, known_hosts-политика — pure-логика под юнит-тестами, end-to-end проверено против localhost sshd**; **переиспользуемый примитив `SshConnection`**; **multi-hop jump-host chains через вложенный `direct-tcpip` для shell И SFTP** (`SshConnection::connect_via_jump_chain`, `Connector::connect_*_via_jump_chain`, `MAX_JUMP_CHAIN=8`, CLI `ssh-chain-connect`/`sftp-chain-ls` + single-hop `ssh-jump-connect`/`sftp-jump-ls`); **`Connector` trait с методами jump/chain/SFTP**; **оркестрация jump-chains в `AppCore`** (`connect`/`connect_sftp` разворачивают цепочку gateway-профилей из `ProfileStore` + резолвят секреты, cycle/depth-проверки, pure-тесты в default-сборке); **tunnel driver `RusshTunnelDriver`** (фича `ssh-russh`) — **все три вида форвардинга**: **local `-L`** (CLI `tunnel-local`), **dynamic SOCKS5 `-D`** (CLI `tunnel-socks`, парсер `socks5.rs` в default-сборке) и **remote `-R`** (CLI `tunnel-remote` direct + `tunnel-remote-chain` через jump-chain — `tcpip-forward` на FINAL TARGET); **SSH agent forwarding для shell** (`SshSettings.agent_forwarding`, флаг `--agent-forwarding`, default off; direct и через jump-chain — только target shell); подсветка/alt-screen/PTY(feature); local-shell транспорт через `AppCore` (`LocalShellConnector`/`LocalPtySession`, фича `local-pty`, тест с реальным PTY); менеджер туннелей + mock + тесты; HTTP + scheduler мини-серверы; `ui-common` (app/safety/multiexec/macros/conflict); CLI (+ команды `local-shell`, `ssh-connect`, `ssh-jump-connect`, `ssh-chain-connect`, `tunnel-local`, `tunnel-socks`, `tunnel-remote`, `tunnel-remote-chain`, `sftp-ls`, `sftp-jump-ls`, `sftp-chain-ls`); qt/gtk заготовки; xtask; README.

**НЕ сделано:** несколько `-R` на одно SSH-соединение; SQLite-хранилище; любой GUI; вендорные пресеты подсветки; полная SGR-aware подсветка; прочие мини-серверы (TFTP/FTP/SSH/Telnet/NFS/VNC); RDP/VNC-клиенты; финальный credential-UX для CLI (пароли из env — dev-only); отдельный `CredentialRef` для key-passphrase.

**ВАЖНО:** сборка **верифицирована** (`cargo build`/`cargo test --workspace` + `--features ssh-russh` зелёные, 2026-06-16, rustc 1.96; `fmt --check` и `clippy --all-targets` — 0 варнингов на default и на `ssh-russh` для protocols/tunnels/cli/ui-common). Дефолтная сборка чистая (`cargo tree --no-default-features` не содержит russh; default-`tunnels` не тянет `rrs-protocols`, но **содержит SOCKS5-парсер `socks5.rs`**; **`ui-common` не имеет фичи `ssh-russh` — jump-chain-оркестрация трейтовая и собирается/тестируется в default**). `local-pty`-тест прогоняет реальный `/bin/sh`; `ssh-russh` — **7 pure-тестов protocols (вкл. `plan_agent_forwarding`)**; **19 pure-тестов tunnels**; **2 pure-теста cli** (`parse_hop`/`jump_password_env`, фича `ssh-russh`); **18 pure-тестов ui-common (default-сборка)**; **3 pure-теста core** (agent_forwarding default/serde); плюс ignored live: `sftp_roundtrip`, `jump_host_roundtrip`, `sftp_jump_roundtrip`, `jump_chain_roundtrip`, `sftp_jump_chain_roundtrip`, `agent_forwarding_roundtrip` (protocols), `local_tunnel_roundtrip` + `dynamic_socks_roundtrip` + `remote_tunnel_roundtrip` + `remote_tunnel_chain_roundtrip` (tunnels) — все требуют sshd. Первый шаг любой сессии всё равно — `cargo build` + `cargo test --workspace`.

## TODO / Следующие шаги (приоритет v0.2)

1. ~~`cargo build` + `cargo test --workspace`~~ — **готово**, зелёные (см. «Текущий прогресс»). Дефолт остаётся чистым; `portable-pty` 0.8 API подтверждён (используется в `local-pty`).
2. ~~Реализовать `RusshConnector`~~ — **готово** (см. п. ниже). ~~jump-host через `direct-tcpip`~~ — **готово**: `SshConnection::connect_via_jump_host` (single-hop) + `RusshConnector::connect_shell_via_jump`, CLI `ssh-jump-connect`. ~~Вынести общий примитив SSH-сессии~~ — **готово**: `SshConnection` (`establish()` удалён).
3. ~~Провести реальный PTY (`LocalPty`) в адаптер под `RemoteSession`~~ — **готово**: `crates/protocols/src/local.rs` (`LocalShellConnector`/`LocalPtySession`), диспетчеризация по `ProtocolKind` в `AppCore::connect`, фича `local-pty`, CLI-команда `local-shell`. Блокирующие openpty/recv — на `spawn_blocking`.
4. ~~Реальный `TunnelDriver`~~ — **готово (все три вида)**: `RusshTunnelDriver` (`crates/tunnels`, фича `ssh-russh`); `-L` (`tunnel-local`), `-D` (`tunnel-socks`), `-R` (`tunnel-remote`).
5. ~~SFTP через jump-host~~ + ~~интеграция в `AppCore`~~ — **готово**.
6. ~~jump-host цепочки длины > 1~~ — **готово**: `SshConnection::connect_via_jump_chain` (итеративный `hop_to`), `Connector::connect_*_via_jump_chain`, `AppCore::resolve_jump_chain` (обход `jump_host` + cycle/depth), `MAX_JUMP_CHAIN=8`, CLI `ssh-chain-connect`/`sftp-chain-ls`.
7. ~~dynamic SOCKS5 (`-D`)~~ — **готово**: `socks5.rs` (default-сборка) + `forward_dynamic`, CLI `tunnel-socks`.
8. ~~remote (`-R`) форвардинг~~ — **готово**: `SshConnection::request_remote_forward`/`take_forwarded_connections` (handler маршрутизирует `forwarded-tcpip`), `forwarded_loop`/`handle_forwarded` в `russh_driver.rs`, `TunnelSpec::new_remote`, CLI `tunnel-remote`. Поддержан bind-порт 0. Один `-R` на соединение.
9. ~~проброс `agent_forwarding`~~ — **готово**: `open_shell` запрашивает `agent_forward`, `ClientHandler::server_channel_open_agent_forward` проксирует к `$SSH_AUTH_SOCK`; `plan_agent_forwarding` (fail-closed); флаг `--agent-forwarding` у `ssh-connect`/`ssh-jump-connect`/`ssh-chain-connect`; работает через jump-chain (только target shell).
10. ~~remote (`-R`) через jump-chain~~ — **готово**: `RusshTunnelDriver::connect_via_jump_chain` (target connection через `SshConnection::connect_via_jump_chain`, `-R` запрашивается на target), CLI `tunnel-remote-chain`.
11. **Следующее**: (а) отдельный `CredentialRef` для key-passphrase (сейчас stored secret трактуется как пароль); (б) несколько `-R` на одно соединение (роутинг входящих по `connected_port`); (в) agent forwarding для exec (non-shell) если понадобится.
12. `SqliteProfileStore` за трейтом `ProfileStore` + миграции.
13. Qt-скелет: одно окно + одна терминальная вкладка поверх `AppCore`; sidebar — модель поверх `ProfileStore`.

## Ограничения и риски

- Сборка верифицирована (см. «Текущий прогресс»).
- **known_hosts**: проверка делегирована `russh::keys::check_known_hosts` (поддерживает hashed-записи). Поведение: trusted→accept; unknown→strict отказ / non-strict accept+warning; changed→всегда отказ. Если файла нет / нет HOME — трактуется как unknown (fail-closed в strict). **Jump-chain**: КАЖДЫЙ хоп проверяется независимо своей политикой (`ClientHandler::new(ssh, ...)` на хоп); `--insecure` в CLI отключает strict для всех хопов.
- **Agent forwarding**: выключено по умолчанию (`SshSettings.agent_forwarding=false`, `#[serde(default)]` — старые профили загружаются как false). Включается только явно (`--agent-forwarding`). Реализация: запрос `auth-agent-req@openssh.com` на session-канале + прозрачный байтовый прокси входящих `auth-agent@openssh.com` каналов к `$SSH_AUTH_SOCK` (`UnixStream`, `copy_bidirectional`). **Agent-протокол не парсится и не логируется**; приватный ключ НЕ покидает агент — сервер лишь просит подпись. **Риск**: доверенный сервер может использовать forwarded-агент для подписи произвольных запросов, пока соединение живо — включать только для доверенных хостов. Fail-closed, если запрошено без `$SSH_AUTH_SOCK`. Незапрошенный agent-канал (forwarding выключен) закрывается. Unix-only. exec (non-shell) и SFTP/tunnels агент не запрашивают.
- **Jump-chain оркестрация**: `Connector` (трейт) НЕ видит хранилища — разворачивание цепочки gateway-профилей и резолв секретов делает **`AppCore`** (`resolve_jump_chain`: обход `jump_host` от target, `ProfileStore::get_profile` + `resolve_credentials`, реверс в порядок подключения), затем зовёт трейтовый `connect_*_via_jump_chain`. Прямой `Connector::connect_shell` при `jump_host.is_some()` всё ещё возвращает `NotImplemented` (его никто не должен звать в обход `AppCore`). Явные ошибки: gateway not found / not SSH / cycle detected (по `HashSet<Uuid>`, включая target) / chain too deep (`> MAX_JUMP_CHAIN`). `validate_jump_chain_endpoints` (protocols) отбраковывает пустые хосты, adjacent-дубликаты и превышение глубины до открытия сокета. `AppCore::connect_sftp` использует основной SSH-`connector` (SFTP — SSH-only) и НЕ регистрирует runtime-session (SFTP-браузер ≠ shell).
- **Tunnel driver**: `RusshTunnelDriver` держит ОДНО SSH-соединение и форвардит по нему все спеки (поле `ssh_profile_id` не ре-резолвится) — один драйвер на endpoint. `start` диспетчеризует по `forward_mode_for(spec)`: `-L` → локальный listener + фиксированный target; `-D` → локальный SOCKS-listener; `-R` → серверный `tcpip-forward` + `forwarded_loop`. Listener/forwarded-loop + по задаче на соединение; `stop`/`Drop` шлёт broadcast-сигнал и `abort()` loop; для `-R` `stop` дополнительно зовёт `cancel-tcpip-forward`. Дочерние задачи завершаются по сигналу/EOF (`copy_bidirectional`). Нет блокирующего I/O на async-рантайме.
- **Remote (`-R`)**: входящие `forwarded-tcpip` каналы доставляются callback'ом `ClientHandler::server_channel_open_forwarded_tcpip` (protocols) в `mpsc`, receiver берётся драйвером ОДИН раз → **один активный `-R` на SSH-соединение** (второй старт → `Driver`-ошибка; CLI создаёт по соединению на запуск). Remote bind-port `0` поддержан — `tcpip_forward` возвращает выбранный сервером порт (логируется, используется для cancel). Если local-connect к target падает — канал закрывается (drop stream), весь процесс не падает. Ограничения сервера: `AllowTcpForwarding`, `GatewayPorts` (для non-loopback bind), привилегированные порты. Имена хостов/портов (bind/target/originator) — не секреты, логируются на `debug`/`info`. **`-R` через jump-chain**: `RusshTunnelDriver::connect_via_jump_chain` строит target connection через `SshConnection::connect_via_jump_chain` (последний hop = target, со своим `forwarded_rx`/handler), поэтому `tcpip-forward` запрашивается на target, а `forwarded-tcpip` мультиплексируются обратно через цепочку. Один `-R` на (target) соединение — то же ограничение; gateways должны разрешать `direct-tcpip` к следующему hop. CLI: `tunnel-remote-chain`.
- **SOCKS5 (`-D`)**: парсер только серверный, SOCKS5 / NO AUTH / CONNECT / IPv4·domain·IPv6; `read_request` использует bounded `read_exact` (domain ≤ 255), без unbounded-чтения и без `unwrap`. SOCKS success-reply отправляется ТОЛЬКО после успешного `open_forward_stream`; на ошибки шлётся корректный failure-reply (`CommandNotSupported`/`AddressTypeNotSupported`/`HostUnreachable`/`GeneralFailure`). `BND.ADDR` в reply — `0.0.0.0:0` (клиенты его игнорируют для CONNECT). Целевой host из CONNECT (включая domain — DNS-резолв на стороне SSH-сервера, как `curl --socks5-hostname`) не является секретом и логируется только на `debug`.
- **Секрет-гигиена в russh**: пароль/passphrase копируются в `String` для API russh (`authenticate_password`/`load_secret_key`) — это рвёт `zeroize` для копии; копия транзиентна и не логируется. Полный zeroize-aware путь — возможное улучшение.
- Версионно-чувствительные места (за фичами ВЫКЛ): feature-имена и API `keyring` 3.x (`keyring_os.rs`); сигнатура `axum::serve` (дефолтная сборка — есть fallback в `http.rs`). API `portable-pty` 0.8 и `russh` 0.61 / `russh-sftp` 2.3 — **подтверждены** (собираются и проходят тесты; версии закреплены в `Cargo.lock`).
- **Крипто-бэкенд russh — `ring`** (выбран вместо дефолтного `aws-lc-rs`, т.к. nasm в окружении отсутствует). Если понадобится `aws-lc-rs` — установить nasm.
- Linux-first. Windows/macOS — позже, через те же трейты (`CredentialStore` → Windows Credential Manager и т.д.).
- X-сервер в долгосроке — **обёртки** Xephyr/Xvfb/Xwayland, без переписывания Xorg.

## Итог итерации (2026-06-16): remote forwarding (`-R`) через jump-chain

- **Архитектурное наблюдение, почти без нового кода**: `SshConnection::connect_via_jump_chain` уже возвращал полноценный **target** connection (последний `hop_to`→`connect_over_stream` создаёт ему собственный `forwarded_rx` + handler). Значит `request_remote_forward`/`take_forwarded_connections`/`cancel_remote_forward` работают на target's handle «из коробки» — `tcpip-forward` ставится на target (последний hop), `forwarded-tcpip` приходят обратно через цепочку. Protocols НЕ менялся.
- **Tunnels**: добавлен конструктор `RusshTunnelDriver::connect_via_jump_chain(gateways, target, creds)` — тонкая обёртка над `SshConnection::connect_via_jump_chain` + `from_connection`. direct `-R`/`-L`/`-D` и весь `forwarded_loop`/`stop`/cancel не тронуты. Граф `tunnels → protocols → core` сохранён; russh в default не подтянут.
- **CLI**: команда `tunnel-remote-chain` (`--jump HOST:USER` ×N required, `--target HOST:USER`, общий `--key`, `--remote-bind`, `--local-target`, `--insecure`). Пароли по индексу: `NEXTERM_JUMP<i>_PASSWORD` / `NEXTERM_TARGET_PASSWORD`, не печатаются. Печатает chain/target/remote-bind/local-target + caveats. Извлечены чистые `jump_password_env(i)` и переиспользован `parse_hop`.
- **Тесты**: pure `parse_hop`/`jump_password_env` в CLI (фича `ssh-russh`, 2 теста); `forward_mode_dispatch`/`remote_forward_endpoints` в tunnels подтверждают, что direct dispatch не регрессирует. Live `#[ignore]` `remote_tunnel_chain_roundtrip` (`rrs-tunnels`): локальный echo + chain `-R`, probe через `direct-tcpip` к target remote-bind; env `NEXTERM_CHAIN_JUMP1_*` (+ опц. `JUMP2`), `NEXTERM_CHAIN_TARGET_*`, `NEXTERM_REMOTE_CHAIN_TEST_BIND`.
- Проверки зелёные: `fmt --check`; `build`/`test`/`clippy --all-targets` (default + `ssh-russh` для protocols/tunnels/cli; ui-common default) — 0 варнингов. `cargo tree --no-default-features` без russh.
- **Следующий шаг**: отдельный `CredentialRef` для key-passphrase; затем несколько `-R` на одно соединение (роутинг входящих по `connected_port`); параллельно — Qt-скелет поверх `AppCore`.

## Итог итерации (2026-06-16): SSH agent forwarding

- **API russh подтверждён по исходникам** (0.61.2): `Channel::agent_forward(want_reply)` шлёт `auth-agent-req@openssh.com`; входящие агент-каналы приходят в callback `client::Handler::server_channel_open_agent_forward(channel, session)`. Никаких выдуманных методов; новых зависимостей нет (`tokio::net::UnixStream` + `copy_bidirectional`).
- **Реализовано реально (не фейк-флаг)**: `SshConnection` хранит `agent_forwarding` + резолвленный `agent_sock` (из `$SSH_AUTH_SOCK`, только если forwarding включён). `open_shell` через чистую `plan_agent_forwarding(enabled, has_sock)` решает: skip / request / fail-closed-Err. `ClientHandler` проксирует каждый `auth-agent@openssh.com` канал на агент-сокет (`proxy_agent_channel`, `#[cfg(unix)]`), payload не парсится/не логируется. Незапрошенный канал закрывается (handler.agent_sock=None).
- **Безопасность**: default off; fail-closed без агента; приватный ключ не передаётся; задокументирован риск подписи. SFTP/tunnels не запрашивают агент. Через jump-chain — только target shell (gateways не открывают shell, их `agent_forwarding=false`).
- **CLI**: флаг `--agent-forwarding` у `ssh-connect`/`ssh-jump-connect`/`ssh-chain-connect`; ставится только на профиль, чей shell открывается (target). Команды печатают подсказку `ssh-add -l`; секреты не печатаются.
- **Модель**: `SshSettings.agent_forwarding` уже было (`#[serde(default)]`, false). Добавлен `ProtocolError::Agent`. Pure-тесты: `plan_agent_forwarding` (4 случая, protocols), `agent_forwarding_defaults_off`/`old_profile_..._false`/`agent_forwarding_roundtrips` (core, serde back-compat). Live `#[ignore]` `agent_forwarding_roundtrip` (`ssh-add -l` на target; env `NEXTERM_SSH_TEST_*` + `SSH_AUTH_SOCK`; сам skip'ается без сокета).
- Проверки зелёные: `fmt --check`; `build`/`test`/`clippy --all-targets` (default + `ssh-russh` для protocols/tunnels/cli; ui-common default) — 0 варнингов. `cargo tree --no-default-features` без russh.
- **Следующий шаг**: remote (`-R`) через jump-chain; затем отдельный `CredentialRef` для key-passphrase и Qt-скелет поверх `AppCore`.

## Итог итерации (2026-06-16): remote forwarding (`ssh -R`)

- **API russh подтверждён по исходникам** (0.61.2): `Handle::tcpip_forward(addr,port)->u32` (возвращает назначенный порт; `0`→server-chosen), `Handle::cancel_tcpip_forward`, и входящие каналы через callback `client::Handler::server_channel_open_forwarded_tcpip(channel, connected_addr/port, originator_addr/port, session)`. Никаких выдуманных методов.
- **Доставка `forwarded-tcpip` (protocols)**: `ClientHandler` расширен `mpsc::UnboundedSender<ForwardedConnection>` и переопределяет callback — кладёт `channel.into_stream()` + метаданные в канал. `SshConnection` хранит receiver + `request_remote_forward`/`cancel_remote_forward`/`take_forwarded_connections`. `ForwardedConnection` (поля + `DirectTcpipStream`) экспортирован. `-L`/`-D`/jump-chain не затронуты.
- **Driver (`-R`)**: `forward_mode_for` теперь возвращает `Remote{remote_*, local_*}`; `spawn_remote` берёт receiver, запрашивает forward (fail при denied/уже-активном), `forwarded_loop` принимает каналы и `handle_forwarded` дозванивается до `local_target` и `copy_bidirectional`. `stop` отменяет server-side bind. `-L`/`-D` вынесены в `ConnMode` (без паник, exhaustive). `TunnelSpec::new_remote` + `remote_forward_endpoints` (bind=remote, target=local; bind-port 0 ок, local-port≠0).
- **CLI**: `tunnel-remote` (`--remote-bind HOST:PORT`, `--local-target HOST:PORT`, `--key`, `--password-env`, `--insecure`); печатает ssh/remote-bind/local-target и подсказку про `AllowTcpForwarding`/`GatewayPorts`; Ctrl-C → корректный stop; пароль не печатается.
- Тесты: pure `forward_mode_dispatch` (теперь Remote→Remote{} + bad-remote→InvalidSpec), `remote_forward_endpoints_validation` (bind/target/port-0/non-remote), `remote_spec_is_accepted_by_manager`. Live `#[ignore]` `remote_tunnel_roundtrip` (поднимает локальный echo, запрашивает remote bind, проверяет echo через `direct-tcpip` к bind; env `NEXTERM_SSH_TEST_*` + `NEXTERM_REMOTE_TEST_BIND`).
- Проверки зелёные: `fmt --check`; `build`/`test`/`clippy --all-targets` (default + `ssh-russh` для protocols/tunnels/cli; ui-common default) — 0 варнингов. `cargo tree --no-default-features` без russh (с `socks5`).
- **Следующий шаг**: `agent_forwarding` (seam уже есть), затем tunnel-менеджмент/jump-chain `-R` и Qt-скелет поверх `AppCore`.

## Итог итерации (2026-06-16): dynamic SOCKS5 (`ssh -D`)

- **SOCKS5-парсер `crates/tunnels/src/socks5.rs`** (default-сборка, без `russh`, без новых зависимостей): чистые `select_method` / `parse_request` / `encode_reply` + типизированный `Socks5Error` (с `reply_code()`); async `negotiate_method` / `read_request` / `write_reply` поверх любого `AsyncRead+AsyncWrite` с bounded `read_exact`. Поддержка: SOCKS5, NO AUTH (`0x00`), CONNECT (`0x01`), ATYP IPv4/domain/IPv6. Отказ: SOCKS4/4a, user/pass auth, UDP ASSOCIATE, BIND, кривой ATYP/версия/команда, короткий пакет, пустой/длинный domain.
- **Dynamic forwarding в `RusshTunnelDriver`**: `start` теперь диспетчеризует через чистую `forward_mode_for(spec)` (`Local`/`Dynamic`/`Remote→Unsupported`); `forward_dynamic` делает SOCKS-хендшейк → `open_forward_stream(target)` → success-reply → `copy_bidirectional`. Local (`-L`) не тронут; remote (`-R`) остался `Unsupported`. Shutdown/Drop/anti-leak — прежний broadcast+abort механизм. `TunnelSpec::new_dynamic` (target=None).
- **CLI**: `tunnel-socks` (`--ssh-host/-port/-user`, `--key`, `--password-env=NEXTERM_SSH_PASSWORD`, `--bind`, `--insecure`); печатает bind+ssh, подсказку `curl --socks5-hostname`, работает до Ctrl-C, пароль не печатается. `tunnel-local` не тронут.
- Тесты (default-сборка): 11 pure SOCKS5 (greeting accept/reject, CONNECT IPv4/domain/IPv6, BIND/UDP reject, short/bad-atyp reject, reply-encoding, reply-codes) + 2 async-duplex (хендшейк/reject) + `forward_mode_dispatch` (ssh-russh) + `dynamic_spec_is_accepted_by_manager`. Live `#[ignore]`: `dynamic_socks_roundtrip` (`rrs-tunnels`, env `NEXTERM_SSH_TEST_*`).
- Проверки зелёные: `fmt --check`; `build`/`test`/`clippy --all-targets` (default + `ssh-russh` для protocols/tunnels/cli; ui-common default) — 0 варнингов. `cargo tree --no-default-features` без russh (но с `socks5`-модулем).
- **Следующий шаг**: remote (`-R`) форвардинг (серверный `tcpip-forward` через russh `Handle`, не `direct-tcpip` — отдельная итерация); затем `agent_forwarding`; параллельно — Qt-скелет поверх `AppCore`.

## Итог итерации (2026-06-16): multi-hop jump-host chains

- **Multi-hop chain primitive**: `SshConnection::connect_via_jump_chain(&[(&SshSettings,&ResolvedCredentials)], target, target_creds)` — первый gateway по TCP, каждый следующий (и target) через `direct-tcpip` предыдущего (хелпер `hop_to` → `connect_over_stream`). Каждый хоп проходит host-key + auth независимо; ничего не дублируется. `connect_via_jump_host` стал тонкой обёрткой над chain. Пустой slice → прямой connect.
- **Валидация**: `validate_jump_chain_endpoints(gateways, target)` (pure) — лимит `MAX_JUMP_CHAIN=8`, непустые хосты, adjacent-дубликаты (self-loop). `validate_jump_chain` — обёртка для single-hop. Полный cycle-detection (по id) — в `AppCore`.
- **Trait**: добавлены `connect_shell_via_jump_chain` / `connect_sftp_via_jump_chain` (default `NotImplemented`); `JumpHop<'a>` (профиль+creds) + `MAX_JUMP_CHAIN` в `traits.rs` (не feature-gated). Single-hop методы делегируют в chain. `ui-common` остаётся без `russh`.
- **AppCore**: `resolve_jump_chain` идёт от target по `jump_host`, копит `HashSet<Uuid>` (cycle), уважает `MAX_JUMP_CHAIN` (depth), реверсит в порядок подключения `gateway1..N`, резолвит creds каждого хопа; `open_shell`/`connect_sftp` строят `Vec<JumpHop>` и зовут chain-методы. Direct/1-hop/2-hop/3-hop/cycle/depth/not-found/not-SSH — 18 pure-тестов (spy-`Connector`, default-сборка).
- **CLI**: `ssh-chain-connect` и `sftp-chain-ls` (`--jump HOST:USER` ×N в порядке подключения, `--target HOST:USER`, общий `--key`, `--insecure`; пароли `NEXTERM_JUMP<i>_PASSWORD`/`NEXTERM_TARGET_PASSWORD`, не печатаются). Single-hop команды не тронуты.
- Live `#[ignore]`: `jump_chain_roundtrip`, `sftp_jump_chain_roundtrip` (env `NEXTERM_CHAIN_JUMP1_*`/`NEXTERM_CHAIN_JUMP2_*`/`NEXTERM_CHAIN_TARGET_*` + `..._SFTP_PATH`).
- Проверки зелёные: `fmt --check`; `build`/`test`/`clippy --all-targets` (default + `ssh-russh` для protocols/tunnels/cli; ui-common default) — 0 варнингов. `cargo tree --no-default-features` и `-p rrs-ui-common` без russh.
- **Следующий шаг**: `agent_forwarding`, затем remote (`-R`)/dynamic (`-D`) туннели; параллельно — Qt-скелет поверх `AppCore` (`connect`/`connect_sftp` уже chain-ready).

## Итог итерации (2026-06-16): SFTP через jump-host + AppCore-оркестрация

- **SFTP через single-hop jump-host**: `RusshSftp::connect_via_jump(jump, jump_creds, target, target_creds)` поверх существующего `SshConnection::connect_via_jump_host(...).open_sftp()` — auth/known_hosts не дублируются. `RusshSftp::connect` (direct) теперь явно отвергает профиль с `jump_host` (резолв второго хопа — дело оркестратора). CLI: `sftp-ls` (direct) и `sftp-jump-ls` (через gateway).
- **`Connector` trait расширен** default-методами `connect_shell_via_jump` / `connect_sftp` / `connect_sftp_via_jump` (default → `NotImplemented`). `RusshConnector` реализует все; бывший inherent `connect_shell_via_jump` перенесён в trait-impl (CLI обновлён: `use ...Connector`). Это даёт `AppCore` единый trait-объект без russh и без доступа к `ProfileStore`.
- **Оркестрация jump-host в `AppCore`**: `connect(profile)` и новый `connect_sftp(profile)` при `jump_host.is_some()` грузят gateway-профиль из `ProfileStore`, валидируют (SSH? не цепочка?), транзиентно резолвят секреты обоих хопов и зовут трейтовые `connect_*_via_jump`. Будущий GUI подключается через jump одним вызовом. Резолв-ошибки тоже помечают session `Failed`.
- Pure-тесты (default-сборка, без russh): 7 новых в `ui-common` — direct vs jump для shell и SFTP, gateway not found / not SSH / chain > 1, `ssh_jump_host`-хелпер; spy-`Connector` + in-memory `ProfileStore`. Live `#[ignore]`: добавлен `sftp_jump_roundtrip` (protocols; env `NEXTERM_{JUMP,TARGET}_TEST_*` + `NEXTERM_TARGET_TEST_SFTP_PATH`).
- Проверки зелёные: `fmt --check`; `build`/`test`/`clippy --all-targets` (default, вкл. 14 тестов `ui-common`); `build`/`test`/`clippy` для `ssh-russh` на protocols/tunnels/cli — 0 варнингов. `cargo tree --no-default-features` без russh. **`ui-common` намеренно без фичи `ssh-russh`** (интеграция трейтовая) — поэтому `-p rrs-ui-common --features ssh-russh` неприменимо; вместо него гоняется `cargo test -p rrs-ui-common`.
- **Следующий шаг**: jump-цепочки длины > 1 (рекурсия в `SshConnection`/`AppCore::resolve_jump`), затем remote/dynamic туннели и `agent_forwarding`; параллельно — Qt-скелет поверх `AppCore` (`connect`/`connect_sftp` уже UI-ready).

## Итог итерации (2026-06-16): jump-host + tunnel driver

- **Выделен примитив `SshConnection`** в `russh_impl.rs`: единый путь connect (TCP) / connect-via-jump (`connect_over_stream` поверх `direct-tcpip`) / host-key / auth, отдаёт `open_shell`/`open_sftp`/`open_forward_stream`. Приватный `establish()` удалён; shell/SFTP теперь идут через примитив (без копипасты auth/known_hosts).
- **Single-hop jump-host реализован реально** (не `ssh target` в shell): `SshConnection::connect_via_jump_host` открывает `channel_open_direct_tcpip` на gateway к target и поднимает поверх вторую SSH-сессию (`client::connect_stream`); host-key + auth проверяются для обоих хостов. Публичная точка — `RusshConnector::connect_shell_via_jump`. Пользователь получает shell на target. CLI: `ssh-jump-connect` (пароли из `NEXTERM_JUMP_PASSWORD`/`NEXTERM_TARGET_PASSWORD`, ключи `--jump-key`/`--target-key`).
- **Real local-forwarding `TunnelDriver`**: `RusshTunnelDriver` в `crates/tunnels` за фичей `ssh-russh` (optional dep `rrs-protocols`; граф `tunnels → protocols → core` остаётся однонаправленным). Биндит listener, на каждое соединение — `direct-tcpip` + `copy_bidirectional`; shutdown через broadcast + `abort`. Только `-L`; `-R`/`-D` → `Unsupported`. CLI: `tunnel-local` (до Ctrl-C, пароль из `NEXTERM_SSH_PASSWORD`).
- Pure-тесты добавлены: `validate_jump_chain` (protocols), `bind_endpoint`/`local_forward_target`/unsupported-kind (tunnels). Live `#[ignore]`: `jump_host_roundtrip` (protocols), `local_tunnel_roundtrip` (tunnels).
- Проверки зелёные: `fmt --check`; `build`/`test`/`clippy --all-targets` (default); `build`/`test`/`clippy` для `ssh-russh` на protocols/tunnels/cli — 0 варнингов. `cargo tree --no-default-features` без russh; default-`tunnels` без `rrs-protocols`. Локально проверено: connect+host-key+auth-флоу исполняется (clean fail при неавторизованном ключе); live jump/tunnel требуют sshd с авторизованным ключом — не гонялись в этой среде.
- **Следующий шаг**: SFTP через jump-host (`open_sftp` на jump-соединении), затем jump-цепочки длины > 1 и remote/dynamic туннели; параллельно — Qt-скелет поверх `AppCore` и резолв jump-профиля в `AppCore`.

## Итог итерации (2026-06-16): реальный SSH/SFTP

- Подключены `russh` 0.61.2 + `russh-sftp` 2.3.0 за фичей `ssh-russh` (бэкенд `ring`, `default-features = false`). Дефолтная сборка осталась чистой и без сетевых зависимостей.
- `RusshConnector` (shell), `RusshSession` (`RemoteSession`), `RusshSftp` (`SftpClient`) реализованы реально (не псевдокод). Auth-порядок и host-key политика вынесены в чистые функции `plan_auth`/`decide_host_key` с юнит-тестами.
- CLI: команда `ssh-connect` (за фичей), пароль из env (dev-only). Бренд в текстах — Nexterm; внутренние имена (`rrs`/`rrs-*`) пока сохранены.
- Проверено end-to-end против локального sshd: publickey-auth, strict-reject неизвестного хоста, strict-accept доверенного, non-strict+warning, полный SFTP round-trip (`sftp_roundtrip`, `#[ignore]`).
- Проверки зелёные: `cargo fmt --all --check`, `build`/`test`/`clippy` (default + `ssh-russh`), `clippy` 0 варнингов. Репозиторий инициализирован под git.
- **Следующий шаг**: jump-host (`channel_open_direct_tcpip` + `connect_stream` в `connect_via_jump_host`), затем реальный `TunnelDriver` на том же примитиве; параллельно — Qt-скелет поверх `AppCore`.
