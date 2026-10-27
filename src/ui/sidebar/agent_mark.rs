use ratatui::style::Color;

use crate::app::state::Palette;
use crate::detect::Agent;

/// One-column mark that stands in for an agent's name in the sidebar agent
/// rows. This is presentation, so the table lives here instead of in the
/// detection manifests or types.
///
/// Every mark is exactly one display column wide, so no row shifts, and none
/// reuses a status indicator glyph or a fold marker: the leading status glyph
/// and the agent mark have to stay visually distinct.
pub(crate) fn agent_mark(agent: Agent) -> &'static str {
    match agent {
        Agent::Pi => "▲",
        Agent::Claude => "✦",
        Agent::Codex => "◆",
        Agent::Gemini => "✧",
        Agent::Cursor => "◇",
        Agent::Devin => "▼",
        Agent::Antigravity => "✱",
        Agent::Cline => "◈",
        Agent::Omp => "■",
        Agent::Mastracode => "□",
        Agent::OpenCode => "◧",
        Agent::GithubCopilot => "✶",
        Agent::Kimi => "★",
        Agent::Kiro => "☆",
        Agent::Droid => "▰",
        Agent::Amp => "✳",
        Agent::Grok => "✴",
        Agent::Hermes => "❖",
        Agent::Kilo => "▱",
        Agent::Qodercli => "◪",
        Agent::Qwen => "◨",
        Agent::Letta => "✥",
        Agent::Maki => "✤",
        Agent::Muse => "✼",
    }
}

/// Color for `agent_mark`. Palette entries are reused where one fits; the rest
/// are fixed mid-tone values that stay readable on dark and light themes.
pub(crate) fn agent_mark_color(agent: Agent, palette: &Palette) -> Color {
    match agent {
        Agent::Pi => palette.teal,
        Agent::Claude => palette.peach,
        Agent::Codex => palette.overlay1,
        Agent::Gemini => palette.blue,
        Agent::Cursor => palette.mauve,
        Agent::Devin => Color::Rgb(0x4f, 0xa3, 0xd1),
        Agent::Antigravity => Color::Rgb(0x9a, 0x7b, 0xd6),
        Agent::Cline => Color::Rgb(0x4c, 0xae, 0x8f),
        Agent::Omp => Color::Rgb(0xc9, 0x8a, 0x3f),
        Agent::Mastracode => Color::Rgb(0xb0, 0x6a, 0xb3),
        Agent::OpenCode => Color::Rgb(0x5f, 0x9e, 0xa0),
        Agent::GithubCopilot => Color::Rgb(0x8b, 0x93, 0xa8),
        Agent::Kimi => Color::Rgb(0xd1, 0x68, 0x9a),
        Agent::Kiro => Color::Rgb(0x7b, 0x9f, 0x3a),
        Agent::Droid => Color::Rgb(0x3f, 0xa7, 0xa0),
        Agent::Amp => Color::Rgb(0xc0, 0x50, 0x4d),
        Agent::Grok => Color::Rgb(0x6f, 0x7b, 0xa6),
        Agent::Hermes => Color::Rgb(0xa4, 0x70, 0x3f),
        Agent::Kilo => Color::Rgb(0x7c, 0xae, 0x5c),
        Agent::Qodercli => Color::Rgb(0x9b, 0x6f, 0xa0),
        Agent::Qwen => Color::Rgb(0x4f, 0x8c, 0xa8),
        Agent::Letta => Color::Rgb(0xcf, 0x7b, 0xb0),
        Agent::Maki => Color::Rgb(0xc2, 0xa0, 0x4b),
        Agent::Muse => Color::Rgb(0x6f, 0xae, 0x9b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_agent_mark_is_one_column_wide_and_unique() {
        let mut seen = Vec::new();
        for agent in Agent::ALL {
            let mark = agent_mark(agent);
            assert_eq!(
                crate::ui::text::display_width(mark),
                1,
                "{mark} is not one display column wide"
            );
            assert!(!seen.contains(&mark), "{mark} is used by two agents");
            seen.push(mark);
        }
    }

    #[test]
    fn every_agent_has_a_mark_color() {
        let palette = Palette::catppuccin();
        for agent in Agent::ALL {
            assert_ne!(agent_mark_color(agent, &palette), Color::Reset);
        }
    }
}
