use async_openai::{
    types::{
        CreateMessageRequestArgs, CreateRunRequestArgs, CreateThreadRequestArgs, MessageContent,
        RunStatus,
    },
    Client,
};
use flowsnet_platform_sdk::logger;
use tg_flows::{listen_to_update, update_handler, Telegram, UpdateKind};
use std::env;

#[no_mangle]
#[tokio::main(flavor = "current_thread")]
pub async fn on_deploy() {
    logger::init();

    let telegram_token = env::var("telegram_token").expect("telegram_token must be set");
    listen_to_update(telegram_token).await;
}

#[update_handler]
async fn handler(update: tg_flows::Update) {
    logger::init();
    let telegram_token = env::var("telegram_token").expect("telegram_token must be set");
    let tele = Telegram::new(telegram_token);

    if let UpdateKind::Message(msg) = update.kind {
        let text = msg.text().unwrap_or("");
        let chat_id = msg.chat.id;

        let thread_id = match store_flows::get(chat_id.to_string().as_str()) {
            Some(ti) => {
                if text == "/restart" {
                    delete_thread(ti.as_str().unwrap()).await;
                    store_flows::del(chat_id.to_string().as_str());
                    return;
                }
                ti.as_str().unwrap().to_owned()
            },
            None => {
                let ti = create_thread().await;
                store_flows::set(
                    chat_id.to_string().as_str(),
                    serde_json::Value::String(ti.clone()),
                    None,
                );
                ti
            }
        };

        let response = run_message(thread_id.as_str(), String::from(text)).await;
        _ = tele.send_message(chat_id, response);
    }
}

async fn create_thread() -> String {
    let client = Client::new();

    let create_thread_request = CreateThreadRequestArgs::default().build().unwrap();

    match client.threads().create(create_thread_request).await {
        Ok(to) => {
            log::info!("New thread (ID: {}) created.", to.id);
            to.id
        }
        Err(e) => {
            log::error!("Failed to create thread: {:?}", e);
            String::from("Failed to create thread.")
        }
    }
}

async fn delete_thread(thread_id: &str) {
    let client = Client::new();

    match client.threads().delete(thread_id).await {
        Ok(_) => {
            log::info!("Old thread (ID: {}) deleted.", thread_id);
        }
        Err(e) => {
            log::error!("Failed to delete thread: {:?}", e);
        }
    }
}

async fn run_message(thread_id: &str, text: String) -> String {
    let client = Client::new();
    let assistant_id = env::var("ASSISTANT_ID").expect("ASSISTANT_ID must be set");

    if let Err(e) = check_and_wait_for_active_run(&client, thread_id).await {
        log::error!("Failed to wait for active run: {:?}", e);
        return String::from("Failed to wait for active run.");
    }

    let mut create_message_request = CreateMessageRequestArgs::default().build().unwrap();
    create_message_request.content = text;
    if let Err(e) = client.threads().messages(thread_id).create(create_message_request).await {
        log::error!("Failed to create message: {:?}", e);
        return String::from("Failed to create message.");
    }

    let mut create_run_request = CreateRunRequestArgs::default().build().unwrap();
    create_run_request.assistant_id = assistant_id;
    let run_id = match client.threads().runs(thread_id).create(create_run_request).await {
        Ok(res) => res.id,
        Err(e) => {
            log::error!("Failed to create run: {:?}", e);
            return String::from("Failed to create run.");
        }
    };

    let mut result = Some("Timeout");
    for _ in 0..5 {
        tokio::time::sleep(std::time::Duration::from_secs(8)).await;
        let run_object = match client.threads().runs(thread_id).retrieve(run_id.as_str()).await {
            Ok(ro) => ro,
            Err(e) => {
                log::error!("Failed to retrieve run: {:?}", e);
                return String::from("Failed to retrieve run.");
            }
        };

        log::info!("Run object: {:?}", run_object);

        result = match run_object.status {
            RunStatus::Queued | RunStatus::InProgress | RunStatus::Cancelling => {
                continue;
            }
            RunStatus::RequiresAction => Some("Action required for OpenAI assistant"),
            RunStatus::Cancelled => Some("Run is cancelled"),
            RunStatus::Failed => Some("Run is failed"),
            RunStatus::Expired => Some("Run is expired"),
            RunStatus::Completed => None,
        };
        break;
    }

    match result {
        Some(r) => String::from(r),
        None => {
            let mut thread_messages = match client.threads().messages(thread_id).list(&[("limit", "1")]).await {
                Ok(tm) => tm,
                Err(e) => {
                    log::error!("Failed to list messages: {:?}", e);
                    return String::from("Failed to list messages.");
                }
            };

            if let Some(c) = thread_messages.data.pop() {
                let texts: Vec<String> = c.content.into_iter().filter_map(|x| match x {
                    MessageContent::Text(t) => Some(t.text.value),
                    _ => None,
                }).collect();
                texts.join("\n")
            } else {
                String::from("No messages found.")
            }
        }
    }
}

async fn check_and_wait_for_active_run(client: &Client, thread_id: &str) -> Result<(), String> {
    let mut run_active = true;
    for _ in 0..5 {
        let active_runs = match client.threads().runs(thread_id).list(&[("status", "in_progress")]).await {
            Ok(runs) => runs.data,
            Err(e) => return Err(format!("Failed to list runs: {:?}", e)),
        };
        if active_runs.is_empty() {
            run_active = false;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
    if run_active {
        Err(String::from("Run is still active after waiting."))
    } else {
        Ok(())
    }
}
