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
- **`russh` 0.61 + `russh-sftp` 2.3 — за фичей `ssh-russh`** (крипто-бэкенд `ring`, без nasm; `default-features = false` + `flate2`/`rsa`). `RusshConnector`/`RusshSftp` реализованы (shell + SFTP, auth, known_hosts). **Single-hop jump-host реализован для shell И SFTP** через `direct-tcpip` (`SshConnection::connect_via_jump_host`; `RusshSftp::connect_via_jump`). Host-key проверка через встроенный `russh::keys::check_known_hosts`; политика strict/non-strict — чистые функции `decide_host_key`/`plan_auth`/`validate_jump_chain` (юнит-тесты).
- **`Connector` trait расширен** default-методами `connect_shell_via_jump` / `connect_sftp` / `connect_sftp_via_jump` (default → `NotImplemented`). Это позволяет `AppCore` оркестрировать jump-host и SFTP **только через trait-объект**, не зная про russh и не протаскивая `ProfileStore` в `Connector`. `RusshConnector` переопределяет все три; `MockConnector` — нет (default). SFTP-фабрика возвращает `Box<dyn SftpClient>`.
- **Единый SSH-примитив `SshConnection`** (в `russh_impl.rs`): инкапсулирует connect (TCP) / connect через jump (`connect_over_stream` поверх `direct-tcpip`-канала) / host-key / auth и отдаёт `open_shell` / `open_sftp` / `open_forward_stream`. Shell/SFTP/jump/tunnels используют один путь — без копипасты auth/known_hosts. `establish()` удалён (заменён на `SshConnection::connect`).
- **Real `TunnelDriver` (`RusshTunnelDriver`) живёт в `crates/tunnels` за фичей `ssh-russh`** (optional dep `rrs-protocols`). Граф остаётся однонаправленным: `tunnels → protocols → core` (protocols НЕ зависит от tunnels). Реализован **только local-forwarding (`-L`)**; remote/dynamic → `TunnelError::Unsupported`. Альтернатива (драйвер в protocols, реализующий трейт tunnels) отклонена — она развернула бы граф `protocols → tunnels`.
- **`chrono` не используем** — `std::time::SystemTime`.
- **MVP-хранилище — JSON-файл** (`FileProfileStore`, атомарная запись temp+rename). SQLite с миграциями — v0.2, за тем же трейтом.
- **HTTP мини-сервер — `axum` 0.8 + `tower-http` ServeDir.** Если версия не примет голый `Router` в `axum::serve` → `app.into_make_service()` (помечено в `http.rs`) или закрепить `axum = "0.7"`.
- **keyring 3.x** с фичами `sync-secret-service` + `crypto-rust` (pure-Rust крипто, без OpenSSL).

## Важные файлы

- `crates/ui-common/src/app.rs` — **`AppCore`**, точка входа для фронтендов (`connect`, `connect_sftp`, `resolve_credentials`, `set_profile_secret`). **Jump-оркестрация**: `connect`/`connect_sftp` при `jump_host.is_some()` резолвят gateway-профиль из `ProfileStore` + секреты обоих хопов и зовут `Connector::connect_*_via_jump` (`resolve_jump`). Pure-тесты (spy-`Connector` + in-memory `ProfileStore`) — в default-сборке, без `russh`.
- `crates/core/src/model/profile.rs` — доменная модель, `CredentialRef`, `ProtocolSettings` (tagged enum, расширяется новыми вариантами).
- `crates/credentials/src/{secret,backend,memory,keyring_os}.rs` — безопасность секретов.
- `crates/protocols/src/traits.rs` — `Connector` / `RemoteSession` / `SftpClient` / `ResolvedCredentials`.
- `crates/protocols/src/ssh/{mock,russh_impl}.rs` — SSH: рабочий mock + **реальный russh-транспорт** (`RusshConnector`/`RusshSession`/`RusshSftp`, auth-план, host-key политика). **`SshConnection`** — переиспользуемый примитив (connect / connect-via-jump / `open_shell`/`open_sftp`/`open_forward_stream`); `DirectTcpipStream` — тип forward-стрима для туннелей.
- `crates/tunnels/src/{manager,russh_driver}.rs` — `TunnelManager` + трейт `TunnelDriver` + mock; **`RusshTunnelDriver`** (фича `ssh-russh`) — real local-forwarding поверх `SshConnection::open_forward_stream`. Pure-валидация спеков (`bind_endpoint`/`local_forward_target`) под юнит-тестами.
- `crates/protocols/src/local.rs` — local-shell транспорт (фича `local-pty`): `LocalShellConnector` + `LocalPtySession` поверх `rrs_terminal::pty::LocalPty`.
- `crates/terminal/src/{altscreen,highlight,pty}.rs` — терминальная логика.
- `crates/miniservers/src/{service,http,scheduler}.rs` — framework + HTTP + scheduler.
- `apps/cli/src/main.rs` — харнесс для ручной проверки ядра.
- `apps/qt/README.md` — решение по GUI (decision record).

## Текущий прогресс

**Готово (с тестами по ключевым крейтам):** workspace и граф зависимостей; модели/конфиг/события/реестр сессий; `ProfileStore` (JSON); `Secret` + `CredentialStore` (memory + `keyring-os`); трейты протоколов + SSH mock; **реальный SSH/SFTP через `russh` (фича `ssh-russh`): PTY-shell `RusshSession`, `RusshSftp`, auth agent→key→password→keyboard-interactive, known_hosts-политика — pure-логика под юнит-тестами, end-to-end проверено против localhost sshd**; **переиспользуемый примитив `SshConnection`**; **single-hop jump-host через `direct-tcpip` для shell И SFTP** (`connect_via_jump_host`, `RusshSftp::connect_via_jump`, CLI `ssh-jump-connect`/`sftp-jump-ls`); **`Connector` trait с методами jump/SFTP**; **оркестрация jump-host в `AppCore`** (`connect`/`connect_sftp` резолвят gateway-профиль из `ProfileStore` + секреты, pure-тесты в default-сборке); **real local-forwarding `TunnelDriver` (`RusshTunnelDriver`)** (фича `ssh-russh`, CLI `tunnel-local`); подсветка/alt-screen/PTY(feature); local-shell транспорт через `AppCore` (`LocalShellConnector`/`LocalPtySession`, фича `local-pty`, тест с реальным PTY); менеджер туннелей + mock + тесты; HTTP + scheduler мини-серверы; `ui-common` (app/safety/multiexec/macros/conflict); CLI (+ команды `local-shell`, `ssh-connect`, `ssh-jump-connect`, `tunnel-local`, `sftp-ls`, `sftp-jump-ls`); qt/gtk заготовки; xtask; README.

**НЕ сделано:** jump-host **цепочки длины > 1** (single-hop готов; `AppCore::resolve_jump` явно отклоняет `chain > 1`); проброс `agent_forwarding`; **remote (`-R`) и dynamic SOCKS (`-D`) туннели** (`TunnelError::Unsupported`); SQLite-хранилище; любой GUI; вендорные пресеты подсветки; полная SGR-aware подсветка; прочие мини-серверы (TFTP/FTP/SSH/Telnet/NFS/VNC); RDP/VNC-клиенты; финальный credential-UX для CLI (пароли из env — dev-only).

**ВАЖНО:** сборка **верифицирована** (`cargo build`/`cargo test --workspace` + `--features ssh-russh` зелёные, 2026-06-16, rustc 1.96; `fmt --check` и `clippy --all-targets` — 0 варнингов на default и на `ssh-russh` для protocols/tunnels/cli/ui-common). Дефолтная сборка чистая (`cargo tree --no-default-features` не содержит russh; default-`tunnels` не тянет `rrs-protocols`; **`ui-common` не имеет фичи `ssh-russh` — jump-оркестрация трейтовая и собирается/тестируется в default**). `local-pty`-тест прогоняет реальный `/bin/sh`; `ssh-russh` — 5 pure-тестов protocols + 4 pure tunnels; **14 pure-тестов ui-common (jump-оркестрация, default-сборка)**; плюс ignored live: `sftp_roundtrip`, `jump_host_roundtrip`, `sftp_jump_roundtrip` (protocols), `local_tunnel_roundtrip` (tunnels) — все требуют sshd. Первый шаг любой сессии всё равно — `cargo build` + `cargo test --workspace`.

## TODO / Следующие шаги (приоритет v0.2)

1. ~~`cargo build` + `cargo test --workspace`~~ — **готово**, зелёные (см. «Текущий прогресс»). Дефолт остаётся чистым; `portable-pty` 0.8 API подтверждён (используется в `local-pty`).
2. ~~Реализовать `RusshConnector`~~ — **готово** (см. п. ниже). ~~jump-host через `direct-tcpip`~~ — **готово**: `SshConnection::connect_via_jump_host` (single-hop) + `RusshConnector::connect_shell_via_jump`, CLI `ssh-jump-connect`. ~~Вынести общий примитив SSH-сессии~~ — **готово**: `SshConnection` (`establish()` удалён).
3. ~~Провести реальный PTY (`LocalPty`) в адаптер под `RemoteSession`~~ — **готово**: `crates/protocols/src/local.rs` (`LocalShellConnector`/`LocalPtySession`), диспетчеризация по `ProtocolKind` в `AppCore::connect`, фича `local-pty`, CLI-команда `local-shell`. Блокирующие openpty/recv — на `spawn_blocking`.
4. ~~Реальный `TunnelDriver` через `direct-tcpip` russh~~ — **готово для local-forwarding**: `RusshTunnelDriver` (`crates/tunnels`, фича `ssh-russh`), CLI `tunnel-local`. **Осталось**: remote (`-R`) и dynamic SOCKS (`-D`) — сейчас `Unsupported`.
5. ~~SFTP через jump-host~~ — **готово**: `RusshSftp::connect_via_jump`, `Connector::connect_sftp_via_jump`, CLI `sftp-jump-ls`. ~~Интеграция jump-host в `AppCore`/`ProfileStore`~~ — **готово**: `AppCore::connect`/`connect_sftp` резолвят gateway-профиль + секреты и зовут трейтовые `connect_*_via_jump`.
6. **Следующее**: (а) jump-host цепочки длины > 1 (рекурсивный `connect_over_stream`; `AppCore::resolve_jump` сейчас отклоняет `chain > 1`); (б) проброс `agent_forwarding`; (в) remote (`-R`) / dynamic SOCKS (`-D`) туннели; (г) отдельный `CredentialRef` для key-passphrase (сейчас stored secret трактуется как пароль).
7. `SqliteProfileStore` за трейтом `ProfileStore` + миграции.
8. Qt-скелет: одно окно + одна терминальная вкладка поверх `AppCore`; sidebar — модель поверх `ProfileStore`.

## Ограничения и риски

- Сборка верифицирована (см. «Текущий прогресс»).
- **known_hosts**: проверка делегирована `russh::keys::check_known_hosts` (поддерживает hashed-записи). Поведение: trusted→accept; unknown→strict отказ / non-strict accept+warning; changed→всегда отказ. Если файла нет / нет HOME — трактуется как unknown (fail-closed в strict). `agent_forwarding` из модели пока не проводится в канал — TODO. **Jump-host**: оба хоста проверяются независимо своей политикой (`ClientHandler::new(ssh)` на каждый хоп); `--insecure` в CLI отключает strict для обоих.
- **Jump-host (single-hop) оркестрация**: `Connector` (трейт) НЕ видит хранилища — резолв gateway-профиля и секретов обоих хопов делает **`AppCore`** (`resolve_jump` → `ProfileStore::get_profile` + `resolve_credentials`), затем зовёт трейтовый `connect_shell_via_jump` / `connect_sftp_via_jump`. Прямой `Connector::connect_shell` при `jump_host.is_some()` всё ещё возвращает `NotImplemented` (его никто не должен звать в обход `AppCore`). Явные ошибки: gateway not found / not SSH / chain > 1. `validate_jump_chain` (protocols) отбраковывает пустые/совпадающие endpoints до открытия сокета. `AppCore::connect_sftp` использует основной SSH-`connector` (SFTP — SSH-only) и НЕ регистрирует runtime-session (SFTP-браузер ≠ shell).
- **Tunnel driver**: `RusshTunnelDriver` держит ОДНО SSH-соединение и форвардит по нему все спеки (поле `ssh_profile_id` не ре-резолвится) — один драйвер на endpoint. Один accept-loop + по задаче на соединение; `stop`/`Drop` шлёт broadcast-сигнал и `abort()` accept-loop; дочерние forward-задачи завершаются по сигналу/EOF (`copy_bidirectional`). Нет блокирующего I/O на async-рантайме.
- **Секрет-гигиена в russh**: пароль/passphrase копируются в `String` для API russh (`authenticate_password`/`load_secret_key`) — это рвёт `zeroize` для копии; копия транзиентна и не логируется. Полный zeroize-aware путь — возможное улучшение.
- Версионно-чувствительные места (за фичами ВЫКЛ): feature-имена и API `keyring` 3.x (`keyring_os.rs`); сигнатура `axum::serve` (дефолтная сборка — есть fallback в `http.rs`). API `portable-pty` 0.8 и `russh` 0.61 / `russh-sftp` 2.3 — **подтверждены** (собираются и проходят тесты; версии закреплены в `Cargo.lock`).
- **Крипто-бэкенд russh — `ring`** (выбран вместо дефолтного `aws-lc-rs`, т.к. nasm в окружении отсутствует). Если понадобится `aws-lc-rs` — установить nasm.
- Linux-first. Windows/macOS — позже, через те же трейты (`CredentialStore` → Windows Credential Manager и т.д.).
- X-сервер в долгосроке — **обёртки** Xephyr/Xvfb/Xwayland, без переписывания Xorg.

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
