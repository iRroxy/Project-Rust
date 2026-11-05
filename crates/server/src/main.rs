use ai_core::SystemInfo;

#[tokio::main]
async fn main() {
    let info = SystemInfo::current();
    let json_output = serde_json::to_string_pretty(&info).expect("Failed to serialize SystemInfo");

    println!("Server core initialized successfully:\n{}", json_output);
}
