use anyhow::Context;

pub async fn get_json(base: &str, path: &str) -> anyhow::Result<()> {
    let base = base.trim_end_matches('/');
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base}{path}"))
        .send()
        .await
        .context("request")?;
    println!("{}", resp.text().await?);
    Ok(())
}

pub async fn list(
    base: &str,
    path: &str,
    camera: Option<&str>,
    limit: i64,
) -> anyhow::Result<()> {
    let base = base.trim_end_matches('/');
    let mut url = format!("{base}{path}?limit={limit}");
    if let Some(c) = camera {
        url.push_str(&format!("&camera={c}"));
    }
    let client = reqwest::Client::new();
    let body = client.get(url).send().await?.text().await?;
    println!("{body}");
    Ok(())
}
