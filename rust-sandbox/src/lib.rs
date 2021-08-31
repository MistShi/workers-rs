use blake2::{Blake2b, Digest};
use serde::{Deserialize, Serialize};
use worker::*;

mod counter;
mod test;
mod utils;

#[derive(Deserialize, Serialize)]
struct MyData {
    message: String,
    #[serde(default)]
    is: bool,
    #[serde(default)]
    data: Vec<u8>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ApiData {
    user_id: i32,
    title: String,
    completed: bool,
}

#[derive(Serialize)]
struct User {
    id: String,
    timestamp: u64,
    date_from_int: String,
    date_from_str: String,
}

fn handle_a_request(req: Request, _env: Env, _params: RouteParams) -> Result<Response> {
    Response::ok(&format!(
        "req at: {}, located at: {:?}, within: {}",
        req.path(),
        req.cf().coordinates().unwrap_or_default(),
        req.cf().region().unwrap_or("unknown region".into())
    ))
}

#[event(fetch)]
pub async fn main(req: Request, env: Env) -> Result<Response> {
    utils::set_panic_hook();

    let mut router = Router::new();

    router.get("/request", handle_a_request)?;
    router.post("/headers", |req, _, _| {
        let mut headers: http::HeaderMap = req.headers().into();
        headers.append("Hello", "World!".parse().unwrap());

        Response::ok("returned your headers to you.").map(|res| res.with_headers(headers.into()))
    })?;

    router.on_async("/formdata-name", |mut req, _env, _params| async move {
        let form = req.form_data().await?;
        const NAME: &str = "name";
        let bad_request = Response::error("Bad Request", 400);

        if !form.has(NAME) {
            return bad_request;
        }

        let names: Vec<String> = form
            .get_all(NAME)
            .unwrap_or_default()
            .into_iter()
            .map(|entry| match entry {
                FormEntry::Field(s) => s,
                FormEntry::File(f) => f.name(),
            })
            .collect();
        if names.len() > 1 {
            return Response::from_json(&serde_json::json!({ "names": names }));
        }

        if let Some(value) = form.get(NAME) {
            match value {
                FormEntry::Field(v) => Response::from_json(&serde_json::json!({ NAME: v })),
                _ => bad_request,
            }
        } else {
            bad_request
        }
    })?;

    #[derive(Deserialize, Serialize)]
    struct FileSize {
        name: String,
        size: u32,
    }
    router.post_async("/formdata-file-size", |mut req, env, _| async move {
        let form = req.form_data().await?;

        if let Some(entry) = form.get("file") {
            return match entry {
                FormEntry::File(file) => {
                    let kv = env.kv("FILE_SIZES")?;

                    // create a new FileSize record to store
                    let b = file.bytes().await?;
                    let record = FileSize {
                        name: file.name(),
                        size: b.len() as u32,
                    };

                    // hash the file, and use result as the key
                    let mut hasher = Blake2b::new();
                    hasher.update(b);
                    let hash = hasher.finalize();
                    let key = hex::encode(&hash[..]);

                    // serialize the record and put it into kv
                    let val = serde_json::to_string(&record)?;
                    kv.put(&key, val)?.execute().await?;

                    // list the default number of keys from the namespace
                    Response::from_json(&kv.list().execute().await?.keys)
                }
                _ => Response::error("Bad Request", 400),
            };
        }

        Response::error("Bad Request", 400)
    })?;

    router.get_async("/formdata-file-size/:hash", |_, env, params| async move {
        if let Some(hash) = params.get("hash") {
            let kv = env.kv("FILE_SIZES")?;
            return match kv.get(&hash).await? {
                Some(val) => Response::from_json(&val.as_json::<FileSize>()?),
                None => Response::error("Not Found", 404),
            };
        }

        Response::error("Bad Request", 400)
    })?;

    router.post_async("/post-file-size", |mut req, _, _| async move {
        let bytes = req.bytes().await?;
        Response::ok(&format!("size = {}", bytes.len()))
    })?;

    router.on("/user/:id/test", |req, _env, params| {
        if !matches!(req.method(), Method::Get) {
            return Response::error("Method Not Allowed", 405);
        }
        if let Some(id) = params.get("id") {
            return Response::ok(format!("TEST user id: {}", id));
        }

        Response::error("Error", 500)
    })?;

    router.on("/user/:id", |_req, _env, params| {
        if let Some(id) = params.get("id") {
            return Response::from_json(&User {
                id: id.to_string(),
                timestamp: Date::now().as_millis(),
                date_from_int: Date::new(DateInit::Millis(1234567890)).to_string(),
                date_from_str: Date::new(DateInit::String(
                    "Wed Jan 14 1980 23:56:07 GMT-0700 (Mountain Standard Time)".into(),
                ))
                .to_string(),
            });
        }

        Response::error("Bad Request", 400)
    })?;

    router.post("/account/:id/zones", |_, _, params| {
        Response::ok(format!(
            "Create new zone for Account: {}",
            params.get("id").unwrap_or(&"not found".into())
        ))
    })?;

    router.get("/account/:id/zones", |_, _, params| {
        Response::ok(format!(
            "Account id: {}..... You get a zone, you get a zone!",
            params.get("id").unwrap_or(&"not found".into())
        ))
    })?;

    router.on_async("/async", |mut req, _env, _params| async move {
        Response::ok(format!("Request body: {}", req.text().await?))
    })?;

    router.on_async("/fetch", |_req, _env, _params| async move {
        let req = Request::new("https://example.com", Method::Post)?;
        let resp = Fetch::Request(&req).send().await?;
        let resp2 = Fetch::Url("https://example.com").send().await?;
        Response::ok(format!(
            "received responses with codes {} and {}",
            resp.status_code(),
            resp2.status_code()
        ))
    })?;

    router.on_async("/fetch_json", |_req, _env, _params| async move {
        let data: ApiData = Fetch::Url("https://jsonplaceholder.typicode.com/todos/1")
            .send()
            .await?
            .json()
            .await?;
        Response::ok(format!(
            "API Returned user: {} with title: {} and completed: {}",
            data.user_id, data.title, data.completed
        ))
    })?;

    router.on_async("/proxy_request/*url", |_req, _env, params| async move {
        let url = params
            .get("url")
            .unwrap()
            .strip_prefix('/')
            .unwrap();

        Fetch::Url(url).send().await
    })?;

    router.on_async("/durable/:id", |_req, env, _params| async move {
        let namespace = env.durable_object("COUNTER")?;
        let stub = namespace.id_from_name("A")?.get_stub()?;
        stub.fetch_with_str("/").await
    })?;

    router.get("/secret", |_req, env, _params| {
        Response::ok(env.secret("SOME_SECRET")?.to_string())
    })?;

    router.get("/var", |_req, env, _params| {
        Response::ok(env.var("SOME_VARIABLE")?.to_string())
    })?;

    router.post_async("/kv/:key/:value", |_req, env, params| async move {
        let kv = env.kv("SOME_NAMESPACE")?;
        if let Some(key) = params.get("key") {
            if let Some(value) = params.get("value") {
                kv.put(&key, value)?.execute().await?;
            }
        }

        Response::from_json(&kv.list().execute().await?)
    })?;

    router.get("/bytes", |_, _, _| {
        Response::from_bytes(vec![1, 2, 3, 4, 5, 6, 7])
    })?;

    router.post_async("/api-data", |mut req, _, _| async move {
        let data = req.bytes().await?;
        let mut todo: ApiData = serde_json::from_slice(&data)?;

        unsafe { todo.title.as_mut_vec().reverse() };

        console_log!("todo = (title {}) (id {})", todo.title, todo.user_id);

        Response::from_bytes(serde_json::to_vec(&todo)?)
    })?;

    router.run(req, env).await
}
