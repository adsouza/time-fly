//! The region bench's banner (owner request): a translucent panel at the top centre naming
//! where the bench is: the region (`Sundaland · 21 thousand years ago · 4 of 9`), the globe pan
//! (`Globe · Beringia → Rockies ice margin`), the next stop while on the globe (`Next: …`), the
//! memory cycles, and at the end the summary's result. A smaller second line names the path
//! segment while flying. It exists only with the bench plugin, and its text is written only
//! when what it shows changes.

use bevy::prelude::*;
use timefly_core::deep_time::{YearsBp, format_years_bp};

use super::{BENCH_REGIONS, CYCLE_REGION, RegionBench, SCRIPT_SECS, Segment, Stage, stop_count};

/// The banner's root node.
#[derive(Component)]
pub struct BenchBanner;

/// The banner's main line.
#[derive(Component)]
pub struct BenchBannerText;

/// The banner's second line (the path segment).
#[derive(Component)]
pub struct BenchBannerDetail;

/// The panel's colour: about 55% black.
pub const BANNER_COLOR: Color = Color::srgba(0.0, 0.0, 0.0, 0.55);

/// What the banner shows; compared each frame, so text is written only when it changes.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BannerKey {
    /// On the globe before region `usize` (settling, loading or after a leave).
    Next(usize),
    /// In region `usize`, flying segment `Option<Segment>` (none before the path starts).
    In(usize, Option<Segment>),
    /// The globe pan into region `usize`.
    Pan(usize),
    /// Memory cycle `u32` (0-based) of `cycles`.
    Cycle(u32, u32),
    /// The bench has finished: whether it passed.
    Finished(bool),
}

/// The stop number (1-based) of region `region`, the globe pans counted as stops.
pub fn stop_number(region: usize) -> usize {
    region
        + 1
        + BENCH_REGIONS[..=region.min(BENCH_REGIONS.len() - 1)]
            .iter()
            .filter(|r| r.pan_from_previous)
            .count()
}

/// The stop shown for a region, as `Name · age · k of n`.
fn stop(region: usize) -> String {
    let r = &BENCH_REGIONS[region];
    format!(
        "{} · {} · {} of {}",
        r.name,
        format_years_bp(YearsBp(r.arg.years)),
        stop_number(region),
        stop_count()
    )
}

/// The banner's lines for `key`: the main line and the optional second one.
pub fn banner_lines(key: BannerKey) -> (String, Option<String>) {
    match key {
        BannerKey::Next(region) => (format!("Next: {}", stop(region)), None),
        BannerKey::In(region, segment) => (stop(region), segment.map(|s| s.label().to_string())),
        BannerKey::Pan(region) => (
            format!(
                "Globe · {} → {}",
                BENCH_REGIONS[region.saturating_sub(1)].name,
                BENCH_REGIONS[region].name
            ),
            Some(format!("{} of {}", stop_number(region) - 1, stop_count())),
        ),
        BannerKey::Cycle(cycle, cycles) => (
            format!("Memory cycles · {CYCLE_REGION} · {} of {cycles}", cycle + 1),
            None,
        ),
        BannerKey::Finished(pass) => (
            format!(
                "Region bench finished: {}",
                if pass { "PASS" } else { "FAIL" }
            ),
            None,
        ),
    }
}

/// What the banner shows for the bench's current stage.
pub(super) fn banner_key(bench: &RegionBench) -> BannerKey {
    use super::Next;
    let next = |n: Next| match n {
        Next::Region(r) => BannerKey::Next(r),
        Next::Cycle(c) => BannerKey::Cycle(c, bench.config.cycles),
        Next::Summary => {
            BannerKey::Cycle(bench.config.cycles.saturating_sub(1), bench.config.cycles)
        }
    };
    match bench.stage {
        Stage::WaitingForGlobe | Stage::Settling { .. } => BannerKey::Next(0),
        Stage::Loading { region, .. } => BannerKey::Next(region),
        Stage::Poised { region, .. } => BannerKey::In(region, None),
        Stage::Flying { region, .. } => BannerKey::In(region, Some(bench.segment)),
        Stage::Panning { region, .. } => BannerKey::Pan(region),
        Stage::Leaving { next: n, .. } | Stage::Resting { next: n, .. } => next(n),
        Stage::CycleLoading { cycle, .. } | Stage::CyclePoised { cycle, .. } => {
            BannerKey::Cycle(cycle, bench.config.cycles)
        }
        Stage::Done => BannerKey::Finished(bench.passed.unwrap_or(false)),
    }
}

/// Spawns the banner (the bench plugin only).
pub(super) fn spawn_banner(mut commands: Commands) {
    commands
        .spawn((
            Name::new("Region bench banner"),
            BenchBanner,
            // Above the fade overlay (-1) and the captions; the "Descending..." caption sits at
            // 40% of the height and the fallback notice at 8%, below this.
            GlobalZIndex(10),
            Pickable::IGNORE,
            Node {
                position_type: PositionType::Absolute,
                left: px(0),
                right: px(0),
                top: px(10),
                justify_content: JustifyContent::Center,
                ..default()
            },
        ))
        .with_children(|root| {
            root.spawn((
                Node {
                    flex_direction: FlexDirection::Column,
                    align_items: AlignItems::Center,
                    padding: UiRect::axes(px(16), px(6)),
                    border_radius: BorderRadius::all(px(8)),
                    ..default()
                },
                BackgroundColor(BANNER_COLOR),
                Pickable::IGNORE,
            ))
            .with_children(|panel| {
                panel.spawn((
                    BenchBannerText,
                    Text::new(""),
                    TextFont::from_font_size(20.0),
                    TextColor(Color::WHITE),
                    Pickable::IGNORE,
                ));
                panel.spawn((
                    BenchBannerDetail,
                    Text::new(""),
                    TextFont::from_font_size(14.0),
                    TextColor(Color::WHITE.with_alpha(0.8)),
                    Node {
                        display: Display::None,
                        ..default()
                    },
                    Pickable::IGNORE,
                ));
            });
        });
}

/// The banner's second line and its node.
type DetailQuery<'w, 's> = Query<
    'w,
    's,
    (&'static mut Text, &'static mut Node),
    (With<BenchBannerDetail>, Without<BenchBannerText>),
>;

/// Writes the banner's text when what it shows changes.
pub(super) fn update_banner(
    bench: Res<RegionBench>,
    mut shown: Local<Option<BannerKey>>,
    mut main: Query<&mut Text, (With<BenchBannerText>, Without<BenchBannerDetail>)>,
    mut detail: DetailQuery,
) {
    let key = banner_key(&bench);
    if *shown == Some(key) {
        return;
    }
    let (line, second) = banner_lines(key);
    for mut text in &mut main {
        text.0.clone_from(&line);
    }
    for (mut text, mut node) in &mut detail {
        let display = if second.is_some() {
            Display::Flex
        } else {
            Display::None
        };
        if node.display != display {
            node.display = display;
        }
        text.0 = second.clone().unwrap_or_default();
    }
    info!(banner = line, "region bench banner");
    *shown = Some(key);
}

/// Used by the bench to track the current segment for the banner.
pub(super) fn segment_at(script_s: f32) -> Segment {
    Segment::at(script_s.min(SCRIPT_SECS))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bench::region::{RegionBenchConfig, RegionBenchPlugin};

    #[test]
    fn stops_count_the_pan() {
        assert_eq!(stop_count(), 9);
        let numbers: Vec<_> = (0..BENCH_REGIONS.len()).map(stop_number).collect();
        // Pangaea, Red Sea, Himalayas, Sundaland, Japan, Beringia, (pan = 7), Rockies, Panama.
        assert_eq!(numbers, [1, 2, 3, 4, 5, 6, 8, 9]);
    }

    #[test]
    fn the_banner_names_each_stop() {
        assert_eq!(
            banner_lines(BannerKey::In(3, None)),
            (
                "Sundaland · 21 thousand years ago · 4 of 9".to_string(),
                None
            )
        );
        assert_eq!(
            banner_lines(BannerKey::In(0, Some(Segment::Climb))).1,
            Some(Segment::Climb.label().to_string())
        );
        assert_eq!(
            banner_lines(BannerKey::Next(2)).0,
            "Next: Himalayas · Present · 3 of 9"
        );
        assert_eq!(
            banner_lines(BannerKey::Pan(6)),
            (
                "Globe · Beringia → Rockies ice margin".to_string(),
                Some("7 of 9".to_string())
            )
        );
        assert_eq!(
            banner_lines(BannerKey::Cycle(2, 10)).0,
            "Memory cycles · Himalayas · 3 of 10"
        );
        assert_eq!(
            banner_lines(BannerKey::Finished(true)).0,
            "Region bench finished: PASS"
        );
        assert_eq!(
            banner_lines(BannerKey::Finished(false)).0,
            "Region bench finished: FAIL"
        );
        // The ages come from the slider's formatter.
        assert!(
            banner_lines(BannerKey::In(0, None))
                .0
                .contains(&format_years_bp(YearsBp(250.0e6)))
        );
    }

    fn banners(with_bench: bool) -> usize {
        use crate::region::{RegionGenerated, RegionLeft, RegionRequested};
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_message::<RegionGenerated>()
            .add_message::<RegionRequested>()
            .add_message::<RegionLeft>();
        if with_bench {
            app.add_plugins(RegionBenchPlugin(RegionBenchConfig::new(60, false)));
        }
        app.update();
        let world = app.world_mut();
        world.query::<&BenchBanner>().iter(world).count()
    }

    #[test]
    fn the_banner_exists_only_with_the_bench() {
        assert_eq!(banners(true), 1);
        assert_eq!(banners(false), 0);
    }
}
