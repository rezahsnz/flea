use std::process::{Stdio, Command};
use std::io::Read;
use std::os::unix::process::CommandExt;

pub fn run_docker (cmd: Vec<&str>) -> (i32, String, String) {
    let mut exit_code = -1;
    // println!("running docker with command `{cmd}`");
    let mut child = Command::new("docker")
        // .current_dir(dir)
        // .arg("-c")
        // .arg(cmd)
        .args(cmd)
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn().expect("failed to start docker.");
    let exit_status = child.wait().expect("Failed to collect docker status.");                            
    if let Some(ecode) = exit_status.code() {
        exit_code = ecode;         
    } else {
        println!("docker process terminated by signal.");
    }
    let mut stderr_buffer = String::new();
    if let Some(mut stderr_pipe) = child.stderr {
        let _ = stderr_pipe.read_to_string(&mut stderr_buffer);
    }    
    let mut stdout_buffer = String::new();
    if let Some(mut stdout_pipe) = child.stdout {
        let _ = stdout_pipe.read_to_string(&mut stdout_buffer);
    } 

    (
        exit_code,
        stderr_buffer,
        stdout_buffer,
    )    
}