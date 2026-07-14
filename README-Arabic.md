# open-runo

**منصة GraphQL Federation مبنية بلغة Rust خالصة** (Poem وTauri وCosmo ليست أبدًا
اعتماديات مباشرة — تم إعادة تنفيذ وظائفها يدويًا للحفاظ على التوافق فوق tokio+hyper)
— ميزات الخطة المدفوعة من WunderGraph Cosmo، متاحة الآن كمصدر مفتوح.
مزوّدة بذكاء اصطناعي ذاتي التعلّم (بدون الحاجة لعقود مع نماذج لغوية خارجية).

📖 لغات أخرى: [日本語](README-Japan.md) / [English](README-English.md) ·
لدمج المشروع في مشاريع أخرى راجع **[PORTING.md](PORTING.md)**.

## ما هو open-runo؟

مع تزايد الخدمات المصغرة تتكاثر واجهات REST API (جحيم BFF، انفجار الإصدارات
`/v1 /v2`، إدارة نقاط نهاية خارجة عن السيطرة). يحل open-runo هذه المشكلة من
جذورها باستخدام **GraphQL Federation + VersionlessAPI**. الميزات التي يوفرها
WunderGraph Cosmo (المكتوب بلغة Go) فقط في خططه المدفوعة (Launch/Scale/Enterprise)
تم تنفيذها هنا **بالكامل بلغة Rust خالصة، مجانًا، كمصدر مفتوح**.

## مقارنة الميزات

| الميزة | Cosmo المجاني | Cosmo المدفوع | **open-runo** |
|---|:---:|:---:|:---:|
| GraphQL Federation / Schema Registry | ✅ | ✅ | ✅ |
| Persisted Queries | — | ✅ | ✅ **مجانًا** |
| RBAC دقيق التحكم | — | ✅ | ✅ **مجانًا** |
| SSO (OIDC / JWKS RS256) | — | ✅ | ✅ **مجانًا** |
| توفير SCIM 2.0 | — | ✅ | ✅ **مجانًا** |
| سجل تدقيق | — | ✅ | ✅ **مجانًا** |
| حدود الطلبات/الفريق/مدة الاحتفاظ | نعم | مخفّفة | **لا توجد** |

### ميزات حصرية في open-runo

- 🧠 ذكاء اصطناعي ذاتي التعلّم (بدون تكاليف نماذج لغوية خارجية) — تخزين مؤقت تكيّفي لصفحات HTML
- 🔑 KeyGuardian — إدارة كاملة وآلية لمفاتيح API (إصدار/إلغاء عبر SCIM)
- 🗄️ DUAL DATABASE — مزامنة PostgreSQL مع aruaru-db، تحقق وإصلاح تلقائيان
- 📦 نقل واستعادة سهلان — كل البيانات وحالة الذكاء الاصطناعي في ملف JSON واحد قابل للنقل
- 🔀 تحويل المحركات والتكامل الموزّع — تحويل MySQL→PostgreSQL→CockroachDB بأمر واحد
- ⚡ VersionlessAPI — تجنب إنشاء `/v1 /v2` عبر محرك قواعد توافق
- 🖥️ تطبيق سطح مكتب مُصرَّف من Rust إلى WebAssembly (بدون Tauri أو Node.js أو TypeScript)
- ⌨️ واجهة سطر أوامر (`open-runo-cli`)، مكافئة لـ `wgc`، لتسجيل/جلب المخططات
  وحالة الـ federation ومواصفة OpenAPI

## البدء السريع

```bash
git clone https://github.com/aon-co-jp/open-runo
cd open-runo
cargo test --workspace          # 343 اختبارًا (362 مع --all-features)
cargo run -p open-runo-gateway  # تشغيل خادم REST + GraphQL
```

## بنية Workspace (18 حزمة)

يتكون من `open-runo-router` (بوابة REST/مصادقة/تدقيق)،
`open-runo-gateway` (نقطة نهاية GraphQL)، `open-runo-federation` (تركيب المخططات)،
`open-runo-db` (تجريد متعدد المحركات)، وغيرها. التفاصيل: [docs/architecture.md](docs/architecture.md).

## مشاريع ذات صلة

توجد بنية مستهدفة تجمع بين `open-web-server` وهذا المستودع و
`poem-cosmo-tauri` و PostgreSQL و `aruaru-db` و `open-raid-z` (نقل
وكتابة قاعدة بيانات رباعية التكرار، مُنقّح 2026-07-11)، مصممة لمنع فقدان
بيانات العناصر المدفوعة والبيانات المالية/الأوراق المالية في ألعاب
الأونلاين ثلاثية الأبعاد. التفاصيل في `README.md`/`CLAUDE.md` الخاصين بـ
[open-web-server](https://github.com/aon-co-jp/open-web-server).

## الترخيص

Apache-2.0 OR MIT (اختر ما يناسبك). للمساهمة راجع [CONTRIBUTING.md](CONTRIBUTING.md).
