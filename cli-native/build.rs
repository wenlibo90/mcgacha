// 打包内置 Token：把 Token 写进同目录 token.txt（一行）再编译即可开箱即用
// token.txt 已在 .gitignore 中，不会被提交。公开发布请勿内置个人 Token。
use std::fs;

fn main() {
    let token = fs::read_to_string("token.txt").unwrap_or_default();
    println!("cargo:rustc-env=MC_EMBED_TOKEN={}", token.trim());
    println!("cargo:rerun-if-changed=token.txt");
}
