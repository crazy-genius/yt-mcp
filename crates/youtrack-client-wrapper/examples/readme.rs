use youtrack_client_wrapper::YoutrackClientBuilder;

#[tokio::main]
async fn main() {
    let host = std::env::var("YOUTRACK_HOST").expect("YOUTRACK_HOST not found");
    let token = std::env::var("YOUTRACK_TOKEN").expect("set YOUTRACK_TOKEN env variable");

    let client = YoutrackClientBuilder::new()
        .with_base_url(host)
        .with_token(token)
        .build()
        .expect("Failed to build client");

    let users_api = client.users_api();

    let fields_query = vec!["login".into(), "id".into()].into();
    let me = users_api.me(Some(fields_query)).await.expect("query complete");

    match me {
        yt_rs::user::User::Known(known_user) => match known_user {
            yt_rs::user::UserKind::Me(me) => println!("{:?}", me),
            yt_rs::user::UserKind::User(user) => println!("user entry: {:?}", user),
            yt_rs::user::UserKind::VcsUnresolvedUser(unresolved_user) => {
                println!("unresolved entry: {:?}", unresolved_user)
            }
        },
        yt_rs::user::User::Unknown(_) => todo!(),
    }
}
