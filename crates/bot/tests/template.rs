// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! The template a new mod starts from, over a real server.
//!
//! `server --create-mod` writes it out and checks it without a world; this is
//! the other half of Task 16's acceptance: the mod LOADS on a server and its
//! block reaches a player. And the tour does what it says: the action opens
//! the dialog, the button drops a beacon and plays the ping.

use std::path::PathBuf;
use std::time::Duration;

use bot::Bot;
use tiamat_core::identity::{Allowlist, Identity};
use tiamat_core::interest::ViewDistance;
use tiamat_core::proto::DialogEvent;
use tiamat_server::scaffold::{self, NewMod};
use tiamat_server::{ServerHandle, Settings};

/// How long to wait for something the server has to tick before it is true.
const PATIENCE: Duration = Duration::from_secs(30);

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("tiamat-template").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// A server whose only mod is the template, written out as `tour`.
fn start(name: &str) -> ServerHandle {
    let root = scratch(name);
    let mods = root.join("mods");
    std::fs::create_dir_all(&mods).expect("mods dir");
    scaffold::create(&NewMod::with_defaults("tour", None, None, None), &mods).expect("scaffold");
    ServerHandle::start(&Settings {
        bind_addr: "127.0.0.1:0".parse().expect("loopback"),
        world_path: root.join("world"),
        identity_path: None,
        max_players: 4,
        allowlist: Allowlist::open(),
        operators: Vec::new(),
        view_distance: ViewDistance::MINIMUM,
        mods_path: Some(mods),
        enabled_mods: None,
        seed: Some(19),
        rcon: None,
        materials: Vec::new(),
        world_options: Vec::new(),
    })
    .expect("start")
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(future)
}

async fn join(server: &ServerHandle, name: &str) -> Bot {
    let mut bot = Bot::connect(
        server.local_addr(),
        Identity::generate().expect("identity"),
        server.cert_fingerprint(),
    )
    .await
    .expect("connect");
    bot.join(name).await.expect("join");
    bot
}

async fn until(bot: &mut Bot, done: impl Fn(&Bot) -> bool) -> bool {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while tokio::time::Instant::now() < deadline {
        if done(bot) {
            return true;
        }
        let _ = tokio::time::timeout(Duration::from_millis(200), bot.recv()).await;
    }
    done(bot)
}

#[test]
fn the_template_loads_on_a_server_and_its_tour_does_what_it_says() {
    let server = start("tour");
    block_on(async {
        let mut bot = join(&server, "Newcomer").await;

        // Its block reached the player, which is the acceptance line.
        let table = bot.material_table().expect("a material table on join");
        assert!(
            table.iter().any(|def| def.name == "tour:beacon"),
            "the template's block is not in the table: {:?}",
            table.iter().map(|def| def.name.clone()).collect::<Vec<_>>()
        );

        // The word on join.
        assert!(
            until(&mut bot, |bot| bot
                .notices()
                .iter()
                .any(|text| text.starts_with("Tour is loaded")))
            .await,
            "no greeting; notices {:?}",
            bot.notices()
        );

        // The action opens the dialog.
        bot.action("tour:wave", true).await.expect("send");
        assert!(
            until(&mut bot, |bot| bot
                .dialogs()
                .iter()
                .any(|(form, _)| form == "tour:wave"))
            .await,
            "the wave key opened nothing; dialogs {:?}",
            bot.dialogs()
                .iter()
                .map(|(form, _)| form.clone())
                .collect::<Vec<_>>()
        );

        // The button drops a beacon — one more entity than before — plays the
        // ping, and the dialog closes.
        let before = bot.entities().len();
        bot.dialog_event(
            "tour:wave",
            DialogEvent::Pressed {
                name: "mark".to_owned(),
            },
        )
        .await
        .expect("send");
        assert!(
            until(&mut bot, |bot| bot
                .sounds_heard()
                .iter()
                .any(|(sound, _)| sound == "tour:ping"))
            .await,
            "no ping; heard {:?}",
            bot.sounds_heard()
        );
        assert!(
            until(&mut bot, |bot| bot.entities().len() > before).await,
            "no beacon was dropped; {} entities before and {} after",
            before,
            bot.entities().len()
        );
        assert!(
            until(&mut bot, |bot| bot
                .closed_dialogs()
                .iter()
                .any(|form| form == "tour:wave"))
            .await,
            "the dialog stayed open; closed {:?}",
            bot.closed_dialogs()
        );
    });
    assert!(server.stop());
}
