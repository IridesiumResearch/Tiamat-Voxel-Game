
// ---- SCRATCH PROBE (Weather, 2026-09-23): removed after the run. ----
#[test]
#[ignore = "a measurement for the weather mod's tuning; run with --ignored --nocapture"]
fn how_long_weathers_deck_costs_by_knob() {
    let Some(gpu) = gpu() else { return };
    let chunks = scene();
    let mut renderer = prepare(gpu, &chunks, RenderMode::Textured);
    renderer.set_lighting_mode(LightingMode::Beautiful);
    let (w, h) = (1920, 1080);
    let target = Offscreen::new(renderer.gpu(), w, h);
    let deck = |cell: f32, detail: u8, thickness: f32, frequency: f32| tiamat_core::atmosphere::CloudLayer {
        base: 400.0,
        thickness,
        cell,
        detail,
        frequency,
        octaves: 2,
        towers: 0.2,
        drift: [0.5, 0.0],
        evolve: 1.0 / 2400.0,
        colour: [1.0; 3],
        shade: [0.42, 0.44, 0.58],
    };
    let skies: [(&str, f32, f32, f32, f32); 6] = [
        ("clear", 0.15, 0.0, 0.25, 0.0),
        ("clear-no-alto", 0.20, 0.0, 0.0, 0.0),
        ("cloudy", 0.55, 0.40, 0.30, 0.0),
        ("rain", 0.30, 0.85, 0.10, 0.0),
        ("storm", 0.40, 0.70, 0.0, 0.60),
        ("mega", 0.40, 0.70, 0.0, 1.0),
    ];
    let views: [(&str, f64, f32); 3] = [("level", 40.0, 0.0), ("45 up", 40.0, 0.78), ("above", 700.0, -0.6)];
    let qualities = [
        ("Coarse", client::render::clouds::Quality::Coarse),
        ("Normal", client::render::clouds::Quality::Normal),
        ("Fine", client::render::clouds::Quality::Fine),
    ];
    let mut time = |renderer: &mut Renderer, camera: &Camera| {
        for _ in 0..2 {
            let _ = target.capture(renderer, camera);
        }
        let mut samples: Vec<f64> = (0..9)
            .map(|_| {
                let start = std::time::Instant::now();
                let _ = target.capture(renderer, camera);
                start.elapsed().as_secs_f64() * 1000.0
            })
            .collect();
        samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
        samples[4]
    };
    println!("PROBE {w}x{h}, Beautiful, median of 9; 'added' is the deck over a bare sky");
    for (vlabel, height, pitch) in views {
        let mut camera = Camera { position: Position::from_world(24.0, height, 20.0), ..Camera::default() };
        camera.look(0.0, pitch);
        renderer.set_clouds(client::render::clouds::Deck { layer: None, clouds: None, quality: client::render::clouds::Quality::Normal, seed: 4242 });
        let bare = time(&mut renderer, &camera);
        println!("PROBE view {vlabel}: bare {bare:.2} ms");
        // A. Weather's deck as it is, every sky, every tier.
        for (qlabel, quality) in qualities {
            for (sky, cover, strato, alto, cb) in skies {
                renderer.set_clouds(client::render::clouds::Deck {
                    layer: Some(deck(16.0, 2, 200.0, 1.0 / 680.0)),
                    clouds: Some(sky_of(cover, strato, alto, cb)),
                    quality,
                    seed: 4242,
                });
                let added = time(&mut renderer, &camera) - bare;
                println!("PROBE A {vlabel:>6} {qlabel:>6} {sky:>14}: added {added:6.2} ms");
            }
        }
        // B. The knobs, at Normal, under the cloudy sky and the storm.
        let decks: [(&str, f32, u8, f32, f32); 7] = [
            ("current c16 d2 t200 f680", 16.0, 2, 200.0, 1.0 / 680.0),
            ("cell 24", 24.0, 2, 200.0, 1.0 / 680.0),
            ("cell 32", 32.0, 2, 200.0, 1.0 / 680.0),
            ("detail 1", 16.0, 1, 200.0, 1.0 / 680.0),
            ("thickness 140", 16.0, 2, 140.0, 1.0 / 680.0),
            ("freq 1/850 (bigger)", 16.0, 2, 200.0, 1.0 / 850.0),
            ("cell 24, freq 1/850", 24.0, 2, 200.0, 1.0 / 850.0),
        ];
        for (sky, cover, strato, alto, cb) in [skies[2], skies[4]] {
            for (dlabel, cell, detail, thickness, frequency) in decks {
                renderer.set_clouds(client::render::clouds::Deck {
                    layer: Some(deck(cell, detail, thickness, frequency)),
                    clouds: Some(sky_of(cover, strato, alto, cb)),
                    quality: client::render::clouds::Quality::Normal,
                    seed: 4242,
                });
                let added = time(&mut renderer, &camera) - bare;
                println!("PROBE B {vlabel:>6} {sky:>6} {dlabel:>24}: added {added:6.2} ms");
            }
        }
    }
}
