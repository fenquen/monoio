//! Use macro to run async main

#[monoio::main(entries = 512, threads = 0, driver = "legacy")]
async fn main() {
    println!("will sleep about 1 sec");

    let begin = std::time::Instant::now();
    monoio::time::sleep(monoio::time::Duration::from_secs(1)).await;
    let eps = std::time::Instant::now().saturating_duration_since(begin);

    println!("elapsed: {}ms", eps.as_millis());
}

fn main0() {
    let body = async {
        { std::io::_print(format_args_nl!("will sleep about 1 sec")); };
        let begin = std::time::Instant::now();
        monoio::time::sleep(monoio::time::Duration::from_secs(1)).await;
        let eps = std::time::Instant::now().saturating_duration_since(begin);
        { std::io::_print(format_args_nl!("elapsed: {}ms", eps.as_millis())); };
    };

    let threads: Vec<_> = (1..8).map(|_| {
        std::thread::spawn(|| {
            monoio::RuntimeBuilder::<monoio::LegacyDriver>::new().with_entries(512u32).build().unwrap().block_on(
                async {
                    { std::io::_print(format_args_nl!("will sleep about 1 sec")); };
                    let begin = std::time::Instant::now();
                    monoio::time::sleep(monoio::time::Duration::from_secs(1)).await;
                    let eps = std::time::Instant::now().saturating_duration_since(begin);
                    { std::io::_print(format_args_nl!("elapsed: {}ms", eps.as_millis())); };
                });
        })
    }).collect();

    monoio::RuntimeBuilder::<monoio::LegacyDriver>::new().with_entries(512u32).build().expect("Failed building the Runtime").block_on(body);
    threads.into_iter().for_each(|t| { let _ = t.join(); });
}
