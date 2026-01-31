use axum::response::Html;

pub async fn index_handler() -> Html<&'static str> {
    Html(include_str!("../index.html"))
}

pub async fn graph_handler() -> Html<&'static str> {
    Html(include_str!("../graph.html"))
}
