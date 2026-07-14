# open-runo

**Платформа GraphQL Federation, написанная на чистом Rust** (Poem/Tauri/Cosmo
никогда не являются прямыми зависимостями — их функциональность реализована
вручную для совместимости поверх tokio+hyper)
— платные функции WunderGraph Cosmo в виде открытого ПО.
Встроен собственный самообучающийся ИИ (без контрактов с внешними LLM).

📖 Другие языки: [日本語](README-Japan.md) / [English](README-English.md) ·
Для интеграции в другие проекты см. **[PORTING.md](PORTING.md)**.

## Что такое open-runo?

Рост числа микросервисов приводит к разрастанию REST API (ад BFF,
взрыв версий `/v1 /v2`, неуправляемое количество эндпоинтов).
open-runo решает эту проблему в корне с помощью **GraphQL Federation + VersionlessAPI**.
Функции, которые WunderGraph Cosmo (на Go) предлагает только в платных
тарифах (Launch/Scale/Enterprise), здесь **полностью реализованы на чистом Rust,
бесплатно, как OSS**.

## Сравнение функций

| Функция | Cosmo Free | Cosmo Paid | **open-runo** |
|---|:---:|:---:|:---:|
| GraphQL Federation / Schema Registry | ✅ | ✅ | ✅ |
| Persisted Queries | — | ✅ | ✅ **бесплатно** |
| Гранулярный RBAC | — | ✅ | ✅ **бесплатно** |
| SSO (OIDC / JWKS RS256) | — | ✅ | ✅ **бесплатно** |
| SCIM 2.0 провижининг | — | ✅ | ✅ **бесплатно** |
| Аудит-лог | — | ✅ | ✅ **бесплатно** |
| Лимиты запросов/команды/хранения | Есть | Смягчены | **Отсутствуют** |

### Только в open-runo

- 🧠 Самообучающийся ИИ (без затрат на внешние LLM) — адаптивный HTML-кэш, динамический TTL
- 🔑 KeyGuardian — полностью автоматическое управление API-ключами (выпуск/отзыв через SCIM)
- 🗄️ DUAL DATABASE — зеркалирование PostgreSQL + aruaru-db, автопроверка и автовосстановление
- 📦 Простой перенос/восстановление — все данные + состояние ИИ в одном переносимом JSON
- 🔀 Конвертация движков и распределённая интеграция — MySQL→PostgreSQL→CockroachDB одной командой
- ⚡ VersionlessAPI — не нужно создавать `/v1 /v2` благодаря движку правил совместимости
- 🖥️ Десктопное приложение, скомпилированное из Rust в WebAssembly (без Tauri, Node.js и TypeScript)
- ⌨️ CLI (`open-runo-cli`), аналог `wgc`, для регистрации/получения схем,
  статуса federation и спецификации OpenAPI

## Быстрый старт

```bash
git clone https://github.com/aon-co-jp/open-runo
cd open-runo
cargo test --workspace          # 343 тестов (362 с --all-features)
cargo run -p open-runo-gateway  # сервер REST + GraphQL
```

## Структура workspace (18 крейтов)

Состоит из `open-runo-router` (REST-шлюз/аутентификация/аудит),
`open-runo-gateway` (GraphQL-эндпоинт), `open-runo-federation` (композиция схем),
`open-runo-db` (абстракция над несколькими движками БД) и др. Подробнее: [docs/architecture.md](docs/architecture.md).

## Связанные проекты

Существует целевая архитектура, объединяющая `open-web-server` с этим
репозиторием, `poem-cosmo-tauri`, PostgreSQL, `aruaru-db` и `open-raid-z`
(четырёхкратно резервированный транспорт и запись в БД, пересмотрено
2026-07-11), призванная предотвратить потерю данных платных предметов и
финансовых/биржевых данных в 3D онлайн-играх. Подробности см. в
`README.md`/`CLAUDE.md` проекта
[open-web-server](https://github.com/aon-co-jp/open-web-server).

## Лицензия

Apache-2.0 OR MIT (на выбор). Как внести вклад — см. [CONTRIBUTING.md](CONTRIBUTING.md).
