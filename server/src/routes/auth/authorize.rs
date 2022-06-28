use std::{collections::HashMap, sync::Arc};

use axum::extract::Query;
use axum::response::{IntoResponse, Redirect};
use axum::Extension;
use axum_extra::extract::{cookie::Cookie, CookieJar};

use itertools::Itertools;
use postgres_types::ToSql;
use reqwest::StatusCode;
use tokio_postgres::Client;

use crate::api::{get_me_and_friends, get_tokens};
use crate::database::insert_session;
use crate::models::server::ServerState;
use crate::utils::{gen_random_str, hashmap};

pub async fn authorize(
    Query(query_params): Query<HashMap<String, String>>,
    Extension(server_state): Extension<Arc<ServerState>>,
    Extension(db): Extension<Arc<Client>>,
    jar: CookieJar,
) -> Result<impl IntoResponse, impl IntoResponse> {
    let Some(code) = query_params.get("code") else {
        return Err((StatusCode::BAD_REQUEST, "Code is required!"));
    };

    let client = reqwest::Client::new();
    let params: HashMap<&str, &str> = hashmap! {
        "client_id"     => "15638",
        "client_secret" => &server_state.client_secret,
        "code"          => code,
        "grant_type"    => "authorization_code",
        "redirect_uri"  => &server_state.auth_redirect_uri
    };

    let tokens = get_tokens(&client, &params).await?;
    let (user, mut friends) = get_me_and_friends(&client, &tokens).await?;
    let friend_ids: Vec<i32> = friends.iter().map(|user| user.id).collect();

    friends.push(user.clone());

    // BAD CODE WARNING
    let global_ranks: &Vec<i32> = &friends.iter().map(|x| x.statistics.global_rank.unwrap_or(0)).collect();
    let mut index = 0;

    let params: &Vec<&(dyn ToSql + Sync)> = &friends
        .iter()
        .flat_map(|row| {
            index += 1;

            [
                &row.id,
                &row.username as &(dyn ToSql + Sync),
                &global_ranks[index - 1],
                &row.country_code,
                &row.avatar_url,
                &row.cover.url,
            ]
        })
        .collect();

    let query = format!(
        "INSERT INTO users VALUES {} ON CONFLICT DO NOTHING",
        (1..=params.len())
            .tuples()
            .format_with(", ", |(id, username, global_rank, country_code, avatar_url, cover_url), f| {
                f(&format_args!("(${id}, ${username}, ${global_rank}, ${country_code}, ${avatar_url}, ${cover_url})"))
            }),
    );

    if db.execute(&query, params).await.is_err() {
        return Err((StatusCode::INTERNAL_SERVER_ERROR, "Can't add users!"));
    };

    let session_str = gen_random_str();
    insert_session(
        &db,
        &user.id,
        &friend_ids,
        &session_str,
        &tokens.access_token,
        &tokens.refresh_token,
    )
    .await?;

    // probably not a good idea to return access_token and refresh token like this.
    let redirect_uri = format!(
        "{}?access_token={}&refresh_token={}",
        &server_state.redirect_uri, &tokens.access_token, &tokens.refresh_token
    );

    let updated_jar = jar.add(Cookie::new("osu_session", session_str));
    Ok((updated_jar, Redirect::permanent(&redirect_uri)))
}