use std::{convert::Infallible, error::Error};

fn main() -> Result<(), Box<dyn Error>> {
    railq::ui::run_terminal(|_| Ok::<_, Infallible>(()))?;
    Ok(())
}
