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
| `crates/tunnels` | модель и менеджер SSH-туннелей (+ mock-драйвер) |
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
- **`russh` 0.61 + `russh-sftp` 2.3 — за фичей `ssh-russh`** (крипто-бэкенд `ring`, без nasm; `default-features = false` + `flate2`/`rsa`). `RusshConnector`/`RusshSftp` реализованы (shell + SFTP, auth, known_hosts). Jump-host — `NotImplemented` (seam `connect_via_jump_host`). Host-key проверка через встроенный `russh::keys::check_known_hosts`; политика strict/non-strict — чистые функции `decide_host_key`/`plan_auth` (юнит-тесты).
- **`chrono` не используем** — `std::time::SystemTime`.
- **MVP-хранилище — JSON-файл** (`FileProfileStore`, атомарная запись temp+rename). SQLite с миграциями — v0.2, за тем же трейтом.
- **HTTP мини-сервер — `axum` 0.8 + `tower-http` ServeDir.** Если версия не примет голый `Router` в `axum::serve` → `app.into_make_service()` (помечено в `http.rs`) или закрепить `axum = "0.7"`.
- **keyring 3.x** с фичами `sync-secret-service` + `crypto-rust` (pure-Rust крипто, без OpenSSL).

## Важные файлы

- `crates/ui-common/src/app.rs` — **`AppCore`**, точка входа для фронтендов (`connect`, `resolve_credentials`, `set_profile_secret`).
- `crates/core/src/model/profile.rs` — доменная модель, `CredentialRef`, `ProtocolSettings` (tagged enum, расширяется новыми вариантами).
- `crates/credentials/src/{secret,backend,memory,keyring_os}.rs` — безопасность секретов.
- `crates/protocols/src/traits.rs` — `Connector` / `RemoteSession` / `SftpClient` / `ResolvedCredentials`.
- `crates/protocols/src/ssh/{mock,russh_impl}.rs` — SSH: рабочий mock + **реальный russh-транспорт** (`RusshConnector`/`RusshSession`/`RusshSftp`, auth-план, host-key политика).
- `crates/protocols/src/local.rs` — local-shell транспорт (фича `local-pty`): `LocalShellConnector` + `LocalPtySession` поверх `rrs_terminal::pty::LocalPty`.
- `crates/terminal/src/{altscreen,highlight,pty}.rs` — терминальная логика.
- `crates/tunnels/src/manager.rs` — `TunnelManager` + `TunnelDriver` + mock + тесты.
- `crates/miniservers/src/{service,http,scheduler}.rs` — framework + HTTP + scheduler.
- `apps/cli/src/main.rs` — харнесс для ручной проверки ядра.
- `apps/qt/README.md` — решение по GUI (decision record).

## Текущий прогресс

**Готово (с тестами по ключевым крейтам):** workspace и граф зависимостей; модели/конфиг/события/реестр сессий; `ProfileStore` (JSON); `Secret` + `CredentialStore` (memory + `keyring-os`); трейты протоколов + SSH mock; **реальный SSH/SFTP через `russh` (фича `ssh-russh`): PTY-shell `RusshSession`, `RusshSftp`, auth agent→key→password→keyboard-interactive, known_hosts-политика — pure-логика под юнит-тестами, end-to-end проверено против localhost sshd**; подсветка/alt-screen/PTY(feature); local-shell транспорт через `AppCore` (`LocalShellConnector`/`LocalPtySession`, фича `local-pty`, тест с реальным PTY); менеджер туннелей + mock + тесты; HTTP + scheduler мини-серверы; `ui-common` (app/safety/multiexec/macros/conflict); CLI (+ команды `local-shell`, `ssh-connect`); qt/gtk заготовки; xtask; README.

**НЕ сделано:** jump-host цепочки (`direct-tcpip`) — `RusshConnector::connect_via_jump_host` возвращает `NotImplemented`; SQLite-хранилище; любой GUI; реальный `TunnelDriver`; вендорные пресеты подсветки; полная SGR-aware подсветка; прочие мини-серверы (TFTP/FTP/SSH/Telnet/NFS/VNC); RDP/VNC-клиенты; финальный credential-UX для CLI (`ssh-connect` берёт пароль из env — dev-only).

**ВАЖНО:** сборка **верифицирована** (`cargo build`/`cargo test --workspace` + `--features ssh-russh` зелёные, 2026-06-16, rustc 1.96). Дефолтная сборка чистая (без сетевых зависимостей); `local-pty`-тест прогоняет реальный `/bin/sh`; `ssh-russh` — 4 pure-теста + ignored `sftp_roundtrip` (требует sshd). Первый шаг любой сессии всё равно — `cargo build` + `cargo test --workspace`.

## TODO / Следующие шаги (приоритет v0.2)

1. ~~`cargo build` + `cargo test --workspace`~~ — **готово**, зелёные (см. «Текущий прогресс»). Дефолт остаётся чистым; `portable-pty` 0.8 API подтверждён (используется в `local-pty`).
2. ~~Реализовать `RusshConnector`~~ — **готово**: `russh` 0.61 + `russh-sftp` 2.3 за фичей `ssh-russh`; auth agent→key→password→keyboard-interactive; known_hosts с учётом `strict_host_key_checking`; PTY-shell + SFTP. **Осталось**: jump-host через `direct-tcpip` (seam `connect_via_jump_host`) — это же разблокирует п.5. После него можно вынести общий примитив SSH-сессии (сейчас `establish()` приватный в `russh_impl.rs`).
3. ~~Провести реальный PTY (`LocalPty`) в адаптер под `RemoteSession`~~ — **готово**: `crates/protocols/src/local.rs` (`LocalShellConnector`/`LocalPtySession`), диспетчеризация по `ProtocolKind` в `AppCore::connect`, фича `local-pty`, CLI-команда `local-shell`. Блокирующие openpty/recv — на `spawn_blocking`.
4. `SqliteProfileStore` за трейтом `ProfileStore` + миграции.
5. Реальный `TunnelDriver` через `direct-tcpip` russh (переиспользовать примитив SSH-сессии).
6. Qt-скелет: одно окно + одна терминальная вкладка поверх `AppCore`; sidebar — модель поверх `ProfileStore`.

## Ограничения и риски

- Сборка верифицирована (см. «Текущий прогресс»).
- **known_hosts**: проверка делегирована `russh::keys::check_known_hosts` (поддерживает hashed-записи). Поведение: trusted→accept; unknown→strict отказ / non-strict accept+warning; changed→всегда отказ. Если файла нет / нет HOME — трактуется как unknown (fail-closed в strict). `agent_forwarding` из модели пока не проводится в канал — TODO.
- **Секрет-гигиена в russh**: пароль/passphrase копируются в `String` для API russh (`authenticate_password`/`load_secret_key`) — это рвёт `zeroize` для копии; копия транзиентна и не логируется. Полный zeroize-aware путь — возможное улучшение.
- Версионно-чувствительные места (за фичами ВЫКЛ): feature-имена и API `keyring` 3.x (`keyring_os.rs`); сигнатура `axum::serve` (дефолтная сборка — есть fallback в `http.rs`). API `portable-pty` 0.8 и `russh` 0.61 / `russh-sftp` 2.3 — **подтверждены** (собираются и проходят тесты; версии закреплены в `Cargo.lock`).
- **Крипто-бэкенд russh — `ring`** (выбран вместо дефолтного `aws-lc-rs`, т.к. nasm в окружении отсутствует). Если понадобится `aws-lc-rs` — установить nasm.
- Linux-first. Windows/macOS — позже, через те же трейты (`CredentialStore` → Windows Credential Manager и т.д.).
- X-сервер в долгосроке — **обёртки** Xephyr/Xvfb/Xwayland, без переписывания Xorg.

## Итог итерации (2026-06-16): реальный SSH/SFTP

- Подключены `russh` 0.61.2 + `russh-sftp` 2.3.0 за фичей `ssh-russh` (бэкенд `ring`, `default-features = false`). Дефолтная сборка осталась чистой и без сетевых зависимостей.
- `RusshConnector` (shell), `RusshSession` (`RemoteSession`), `RusshSftp` (`SftpClient`) реализованы реально (не псевдокод). Auth-порядок и host-key политика вынесены в чистые функции `plan_auth`/`decide_host_key` с юнит-тестами.
- CLI: команда `ssh-connect` (за фичей), пароль из env (dev-only). Бренд в текстах — Nexterm; внутренние имена (`rrs`/`rrs-*`) пока сохранены.
- Проверено end-to-end против локального sshd: publickey-auth, strict-reject неизвестного хоста, strict-accept доверенного, non-strict+warning, полный SFTP round-trip (`sftp_roundtrip`, `#[ignore]`).
- Проверки зелёные: `cargo fmt --all --check`, `build`/`test`/`clippy` (default + `ssh-russh`), `clippy` 0 варнингов. Репозиторий инициализирован под git.
- **Следующий шаг**: jump-host (`channel_open_direct_tcpip` + `connect_stream` в `connect_via_jump_host`), затем реальный `TunnelDriver` на том же примитиве; параллельно — Qt-скелет поверх `AppCore`.
