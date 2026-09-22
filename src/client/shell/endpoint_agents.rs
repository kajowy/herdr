use super::render::put_text;
use super::*;

pub(super) fn render_collapsed(
    buffer: &mut Buffer,
    area: Rect,
    endpoints: &[ClientShellEndpoint],
    active_endpoint_id: &ClientEndpointId,
    config: &ClientShellConfig,
    hits: &mut ShellHitMap,
) {
    let rows = agent_rows(endpoints, active_endpoint_id, config);
    for (index, row) in rows.into_iter().take(area.height as usize).enumerate() {
        let rect = Rect::new(area.x, area.y + index as u16, area.width, 1);
        if row.agent.focused {
            buffer.set_style(rect, Style::default().bg(config.palette.active_row_bg));
        }
        let initial = row.machine_label.chars().next().unwrap_or('?');
        put_text(
            buffer,
            rect.x,
            rect.y,
            rect.width,
            &format!(
                "{initial}{}",
                status_icon(row.agent.status, config.status_indicators)
            ),
            Style::default()
                .fg(if row.stale {
                    config.palette.overlay0
                } else {
                    status_color(row.agent.status, &config.palette)
                })
                .add_modifier(if row.stale {
                    Modifier::DIM
                } else {
                    Modifier::empty()
                }),
        );
        hits.endpoint_agents
            .push((rect, row.endpoint_id, row.agent.pane_id));
    }
}

pub(super) fn render_expanded(
    buffer: &mut Buffer,
    area: Rect,
    agent_view_label: Option<&str>,
    config: &ClientShellConfig,
    state: &mut super::render::ShellRenderState<'_>,
    hits: &mut ShellHitMap,
) {
    if !super::agent_sidebar::render_agent_panel_header(
        buffer,
        area,
        agent_view_label,
        config,
        hits,
        true,
    ) {
        return;
    }
    let (endpoints, active_endpoint_id) = (state.endpoints, state.active_endpoint_id);
    if config.agent_panel_sort == crate::config::AgentPanelSortConfig::Grouped {
        let lines = grouped_lines(endpoints, active_endpoint_id, |endpoint_id| {
            super::endpoint_sidebar::collapsed_groups_for_endpoint(state, endpoint_id)
        });
        super::agent_sidebar::render_grouped_agent_list(
            buffer,
            Rect::new(
                area.x,
                area.y.saturating_add(3),
                area.width,
                area.height.saturating_sub(3),
            ),
            &lines,
            agent_view_label.map(|_| " no matching agents"),
            config,
            state.agent_scroll,
            state.selected_workspace_id,
            true,
            hits,
        );
        return;
    }
    let rows = agent_rows(endpoints, active_endpoint_id, config);
    let agent_scroll = &mut *state.agent_scroll;
    super::agent_sidebar::render_agent_list(
        buffer,
        area,
        &rows,
        agent_view_label.map(|_| " no matching agents"),
        config,
        agent_scroll,
        hits,
        |row| row.agent.rows.len(),
        |buffer, rect, row, hits| {
            super::agent_sidebar::render_agent_row(buffer, rect, &row.agent, config);
            if row.stale {
                buffer.set_style(
                    rect,
                    Style::default()
                        .fg(config.palette.overlay0)
                        .add_modifier(Modifier::DIM),
                );
            }
            hits.endpoint_agents
                .push((rect, row.endpoint_id.clone(), row.agent.pane_id.clone()));
        },
    );
}

/// Grouped agent panel lines across machines: each machine's spaces in order,
/// limited to the agents the aggregate agent view keeps.
fn grouped_lines<'c>(
    endpoints: &[ClientShellEndpoint],
    active_endpoint_id: &ClientEndpointId,
    collapsed_groups: impl Fn(&ClientEndpointId) -> Option<&'c HashSet<String>>,
) -> Vec<super::agent_sidebar::GroupedAgentLine> {
    let rows = super::aggregate_navigation::aggregate_agent_rows(
        endpoints,
        active_endpoint_id,
        crate::config::AgentPanelSortConfig::Grouped,
    );
    let mut lines = Vec::new();
    for endpoint in super::aggregate_navigation::cached_endpoint_snapshots(endpoints) {
        let agents = rows
            .iter()
            .filter(|row| row.endpoint.endpoint_index == endpoint.endpoint_index)
            .map(|row| row.agent)
            .collect::<Vec<_>>();
        super::agent_sidebar::push_grouped_agent_lines(
            &mut lines,
            super::agent_sidebar::GroupedAgentSource {
                endpoint_id: endpoint.endpoint_id,
                machine: Some(endpoint.label),
                stale: endpoint.stale(),
                active: endpoint.endpoint_id == active_endpoint_id,
            },
            endpoint.snapshot,
            &agents,
            collapsed_groups(endpoint.endpoint_id),
        );
    }
    lines
}

impl ClientShellState {
    pub(super) fn reveal_endpoint_agent(
        &mut self,
        endpoint_id: &ClientEndpointId,
        pane_id: &str,
        body_height: u16,
    ) {
        if body_height == 0 {
            return;
        }
        if self.config.agent_panel_sort == crate::config::AgentPanelSortConfig::Grouped {
            let lines = grouped_lines(&self.endpoints, &self.active_endpoint_id, |endpoint_id| {
                self.collapsed_groups_for_endpoint(endpoint_id)
            });
            let Some(target) = lines.iter().position(|line| {
                &line.endpoint_id == endpoint_id
                    && matches!(
                        &line.kind,
                        super::agent_sidebar::GroupedAgentLineKind::Agent { pane_id: candidate, .. }
                            if candidate == pane_id
                    )
            }) else {
                return;
            };
            self.agent_scroll = super::scroll::list_scroll_start_to_reveal(
                &vec![1; lines.len()],
                &super::agent_sidebar::grouped_line_gaps(&lines),
                body_height,
                self.agent_scroll,
                target,
            );
            return;
        }
        let rows = agent_rows(&self.endpoints, &self.active_endpoint_id, &self.config);
        let Some(target) = rows
            .iter()
            .position(|row| &row.endpoint_id == endpoint_id && row.agent.pane_id == pane_id)
        else {
            return;
        };
        let heights = rows
            .iter()
            .map(|row| row.agent.rows.len().max(1).min(u16::MAX as usize) as u16)
            .collect::<Vec<_>>();
        let mut gaps = vec![self.config.agents.row_gap; rows.len()];
        if let Some(last) = gaps.last_mut() {
            *last = 0;
        }
        self.agent_scroll = super::scroll::list_scroll_start_to_reveal(
            &heights,
            &gaps,
            body_height,
            self.agent_scroll,
            target,
        );
    }
}

struct EndpointAgentRow {
    endpoint_id: ClientEndpointId,
    machine_label: String,
    stale: bool,
    agent: super::agent_sidebar::AgentRow,
}

fn agent_rows(
    endpoints: &[ClientShellEndpoint],
    active_endpoint_id: &ClientEndpointId,
    config: &ClientShellConfig,
) -> Vec<EndpointAgentRow> {
    let mut rendered_rows = endpoints
        .iter()
        .filter_map(|endpoint| {
            endpoint.snapshot.as_deref().map(|snapshot| {
                snapshot
                    .agents
                    .iter()
                    .filter_map(|agent| {
                        super::agent_sidebar::agent_row(
                            snapshot,
                            &agent.pane_id,
                            config,
                            Some(&endpoint.label),
                        )
                    })
                    .map(|agent| ((endpoint.endpoint_id.clone(), agent.pane_id.clone()), agent))
                    .collect::<Vec<_>>()
            })
        })
        .flatten()
        .collect::<HashMap<_, _>>();

    super::aggregate_navigation::aggregate_agent_rows(
        endpoints,
        active_endpoint_id,
        config.agent_panel_sort,
    )
    .into_iter()
    .filter_map(|row| {
        let key = (row.endpoint.endpoint_id.clone(), row.agent.pane_id.clone());
        let mut agent = rendered_rows.remove(&key)?;
        agent.focused &= row.endpoint.endpoint_id == active_endpoint_id;
        Some(EndpointAgentRow {
            endpoint_id: row.endpoint.endpoint_id.clone(),
            machine_label: row.endpoint.label.to_owned(),
            stale: row.endpoint.stale(),
            agent,
        })
    })
    .collect()
}
