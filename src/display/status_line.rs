use {
    super::{Screen, W},
    crate::{app::Status, errors::ProgramError},
    minimad::{Alignment, Composite},
    termimad::{Area, StyledChar},
};

/// write the whole status line (task + status)
pub fn write(
    w: &mut W,
    task: Option<&str>,
    status: &Status,
    area: &Area,
    screen: &Screen,
) -> Result<(), ProgramError> {
    let y = area.top;
    screen.goto(w, area.left, y)?;
    let mut x = area.left;
    if let Some(pending_task) = task {
        let pending_task = format!(" {}… ", pending_task);
        x += pending_task.chars().count() as u16;
        screen.skin.status_job.queue(w, pending_task)?;
    }
    screen.goto(w, x, y)?;
    let skin = if status.error {
        &screen.status_skin.error
    } else {
        &screen.status_skin.normal
    };
    skin.write_inline_on(w, " ")?;
    let remaining_width = (screen.width - x - 1) as usize;
    skin.write_composite_fill(
        w,
        Composite::from_inline(&status.message),
        remaining_width,
        Alignment::Left,
    )?;
    Ok(())
}

/// erase the whole status line
pub fn erase(w: &mut W, area: &Area, screen: &Screen) -> Result<(), ProgramError> {
    screen.goto(w, area.left, area.top)?;
    let sc = StyledChar::new(
        screen.status_skin.normal.paragraph.compound_style.clone(),
        ' ',
    );
    sc.queue_repeat(w, area.width as usize)?;
    Ok(())
}
