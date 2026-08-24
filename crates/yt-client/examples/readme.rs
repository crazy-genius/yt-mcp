use yt_client::Youtrack;

#[tokio::main]
async fn main() {
    let host = std::env::var("YOUTRACK_HOST").expect("YOUTRACK_HOST not found");
    let token = std::env::var("YOUTRACK_TOKEN").expect("set YOUTRACK_TOKEN env variable");

    let service = Youtrack::new(host)
        .expect("Failed to build Youtrack")
        .as_user(&token)
        .expect("Failed to build service");

    for project in service.list_projects().await.expect("query complete") {
        println!("{:?} {:?}", project.short_name, project.name);
    }
}
