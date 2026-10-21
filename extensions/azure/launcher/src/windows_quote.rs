use crate::LaunchError;

const MAX_COMMAND_UNITS: usize = 32_767;

pub fn command_line(arguments: &[&[u16]]) -> Result<Vec<u16>, LaunchError> {
    if arguments.is_empty() {
        return Err(LaunchError::InvalidCommandLine);
    }
    let mut output = Vec::new();
    for (index, argument) in arguments.iter().enumerate() {
        if argument.len() >= MAX_COMMAND_UNITS {
            return Err(LaunchError::InvalidCommandLine);
        }
        if index != 0 {
            push(&mut output, b' ' as u16)?;
        }
        push(&mut output, b'"' as u16)?;
        let mut slashes = 0;
        for &unit in *argument {
            if unit == 0 {
                return Err(LaunchError::InvalidCommandLine);
            }
            if unit == b'\\' as u16 {
                slashes += 1;
                continue;
            }
            let count = if unit == b'"' as u16 { slashes * 2 + 1 } else { slashes };
            for _ in 0..count {
                push(&mut output, b'\\' as u16)?;
            }
            slashes = 0;
            push(&mut output, unit)?;
        }
        for _ in 0..slashes * 2 {
            push(&mut output, b'\\' as u16)?;
        }
        push(&mut output, b'"' as u16)?;
    }
    output.push(0);
    Ok(output)
}

fn push(output: &mut Vec<u16>, unit: u16) -> Result<(), LaunchError> {
    if output.len() >= MAX_COMMAND_UNITS - 1 {
        return Err(LaunchError::InvalidCommandLine);
    }
    output.push(unit);
    Ok(())
}
