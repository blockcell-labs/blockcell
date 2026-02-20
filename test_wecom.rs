use base64::{engine::general_purpose::STANDARD, Engine as _};

fn main() {
    let key = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQ";
    let padded = format!("{}=", key);
    println!("PAD: {:?}", STANDARD.decode(&padded));
    
    use base64::engine::general_purpose::STANDARD_NO_PAD;
    println!("NO_PAD: {:?}", STANDARD_NO_PAD.decode(key));
}
