use base64::{engine::general_purpose::STANDARD as B64, Engine as _};

fn main() {
    let encoding_aes_key = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQ";
    println!("key len: {}", encoding_aes_key.len());
    let res = B64.decode(format!("{}=", encoding_aes_key));
    println!("PAD: {:?}", res);
    
    use base64::engine::general_purpose::STANDARD_NO_PAD;
    let res2 = STANDARD_NO_PAD.decode(encoding_aes_key);
    println!("NO_PAD: {:?}", res2);
}
