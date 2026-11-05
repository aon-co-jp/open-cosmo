//! GraphQL(Federationの問い合わせ)の本番向け防御。
//!
//! 管理操作はgRPCへ移したので、GraphQLは「クライアントの問い合わせ」だけを受ける公開面になる。
//! 本番では次の4つを必ず適用する(`OPEN_RUNO_ENV=production`で既定オン。個別に環境変数で上書き可):
//! 1. **登録済みクエリ(永続クエリ)しか受け付けない**(`pq_mode = Enforce`)。任意のクエリ文字列は拒否。
//! 2. **深さ・複雑さの上限**(`max_depth` / `max_complexity`、async-graphqlの検証段階で拒否)。
//! 3. **イントロスペクション停止**。GraphiQL(`GET /graphql`)とSDL(`/graphql/schema.graphql`)の
//!    自己公開もやめる(スキーマはgRPCの`admin.proto`・リフレクションとは別物の公開面なので、
//!    本番で型情報を自分から配らない)。
//! 4. **エイリアス・バッチで1回の通信に処理を詰め込む攻撃への対策**: JSON配列(バッチ)は拒否、
//!    ルートフィールド数(エイリアス含む)に上限、さらに超過分を流量制限の枠から**余計に消費**する
//!    (1通信=1回分では、エイリアスで同じ処理を何十回も走らせて制限を回避できてしまうため)。
//!
//! 環境変数: `OPEN_RUNO_PQ_MODE`(disabled/allow/enforce)、`OPEN_RUNO_GQL_MAX_DEPTH`、
//! `OPEN_RUNO_GQL_MAX_COMPLEXITY`、`OPEN_RUNO_GQL_MAX_ROOT_FIELDS`、
//! `OPEN_RUNO_GQL_INTROSPECTION`(1/0)、`OPEN_RUNO_GQL_EXPOSE_SCHEMA`(1/0)。

use async_graphql::parser::types::{
    DocumentOperations, ExecutableDocument, Selection, SelectionSet,
};
use open_runo_core::Environment;
use open_runo_persisted_queries::EnforcementMode;

/// GraphQLの防御設定。
#[derive(Debug, Clone)]
pub struct GraphqlSecurity {
    pub pq_mode: EnforcementMode,
    pub max_depth: usize,
    pub max_complexity: usize,
    pub max_root_fields: usize,
    pub introspection: bool,
    /// `GET /graphql`(GraphiQL)と`GET /graphql/schema.graphql`を公開するか。
    pub expose_schema: bool,
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

fn env_bool(name: &str, default: bool) -> bool {
    match std::env::var(name).ok().as_deref().map(str::trim) {
        Some("1" | "true" | "on" | "yes") => true,
        Some("0" | "false" | "off" | "no") => false,
        _ => default,
    }
}

impl GraphqlSecurity {
    /// 環境ごとの既定値(本番は厳しく、開発は緩く)に、環境変数の上書きを重ねる。
    pub fn for_environment(env: Environment) -> Self {
        let prod = env == Environment::Production;
        let pq_default = if prod {
            EnforcementMode::Enforce
        } else {
            EnforcementMode::Allow
        };
        let pq_mode = match std::env::var("OPEN_RUNO_PQ_MODE")
            .unwrap_or_default()
            .to_lowercase()
            .as_str()
        {
            "disabled" => EnforcementMode::Disabled,
            "allow" => EnforcementMode::Allow,
            "enforce" => EnforcementMode::Enforce,
            _ => pq_default,
        };
        Self {
            pq_mode,
            max_depth: env_usize("OPEN_RUNO_GQL_MAX_DEPTH", if prod { 8 } else { 20 }),
            max_complexity: env_usize(
                "OPEN_RUNO_GQL_MAX_COMPLEXITY",
                if prod { 100 } else { 1000 },
            ),
            max_root_fields: env_usize("OPEN_RUNO_GQL_MAX_ROOT_FIELDS", if prod { 8 } else { 50 }),
            introspection: env_bool("OPEN_RUNO_GQL_INTROSPECTION", !prod),
            expose_schema: env_bool("OPEN_RUNO_GQL_EXPOSE_SCHEMA", !prod),
        }
    }

    /// `OPEN_RUNO_ENV`(未設定ならlocal)から決める。
    pub fn from_env() -> Self {
        let env = std::env::var("OPEN_RUNO_ENV")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(Environment::Local);
        Self::for_environment(env)
    }
}

/// クエリ中の**ルートフィールド数**(エイリアス・インラインフラグメント・フラグメント展開を含む)を数える。
/// 同じ処理をエイリアスで何十個も並べる攻撃の検出に使う。パースできなければ`Err`。
pub fn count_root_fields(query: &str) -> Result<usize, String> {
    let doc = async_graphql::parser::parse_query(query).map_err(|e| e.to_string())?;
    let sets: Vec<&SelectionSet> = match &doc.operations {
        DocumentOperations::Single(op) => vec![&op.node.selection_set.node],
        DocumentOperations::Multiple(ops) => {
            ops.values().map(|o| &o.node.selection_set.node).collect()
        },
    };
    Ok(sets.into_iter().map(|s| count_set(s, &doc, 0)).sum())
}

fn count_set(set: &SelectionSet, doc: &ExecutableDocument, depth: usize) -> usize {
    // フラグメントの循環参照で無限再帰しないよう深さを打ち切る(超えた分は数えない=上限判定側で別途拒否される)。
    if depth > 32 {
        return 0;
    }
    set.items
        .iter()
        .map(|sel| match &sel.node {
            Selection::Field(_) => 1,
            Selection::InlineFragment(f) => count_set(&f.node.selection_set.node, doc, depth + 1),
            Selection::FragmentSpread(s) => doc
                .fragments
                .get(&s.node.fragment_name.node)
                .map(|f| count_set(&f.node.selection_set.node, doc, depth + 1))
                .unwrap_or(0),
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_aliases_and_fragments_at_the_root() {
        assert_eq!(count_root_fields("{ health }").unwrap(), 1);
        assert_eq!(
            count_root_fields("{ a: health b: health c: health }").unwrap(),
            3
        );
        assert_eq!(
            count_root_fields("query { ...F } fragment F on QueryRoot { x: health y: health }")
                .unwrap(),
            2
        );
        assert_eq!(
            count_root_fields("{ ... on QueryRoot { a: health b: health } }").unwrap(),
            2
        );
        assert!(count_root_fields("{ health").is_err());
    }

    #[test]
    fn production_defaults_are_strict_and_dev_defaults_are_loose() {
        let p = GraphqlSecurity::for_environment(Environment::Production);
        assert_eq!(p.pq_mode, EnforcementMode::Enforce);
        assert!(!p.introspection && !p.expose_schema);
        assert!(p.max_depth <= 10 && p.max_complexity <= 200 && p.max_root_fields <= 10);
        let d = GraphqlSecurity::for_environment(Environment::Local);
        assert_eq!(d.pq_mode, EnforcementMode::Allow);
        assert!(d.introspection && d.expose_schema);
    }
}
