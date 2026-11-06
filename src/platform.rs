#[cfg(target_os = "macos")]
use anyhow::bail;
use anyhow::Result;

/// Observe live members only. Failure is never interpreted as absence.
#[cfg(target_os = "macos")]
pub fn group_has_live_members(pgid: u32) -> Result<bool> {
    // proc_listpids(PROC_PGRP_ONLY) returns bytes. A retained zombie group leader
    // can make Darwin's killpg return EPERM even after every live member exited.
    let mut pids = vec![0i32; 256];
    loop {
        unsafe {
            *libc::__error() = 0;
        }
        let bytes = unsafe {
            libc::proc_listpids(
                2,
                pgid,
                pids.as_mut_ptr().cast(),
                std::mem::size_of_val(pids.as_slice()) as i32,
            )
        };
        if bytes < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        if bytes == 0 {
            let error = std::io::Error::last_os_error();
            if !matches!(error.raw_os_error(), Some(0 | libc::ESRCH)) {
                return Err(error.into());
            }
            return Ok(false);
        }
        if bytes as usize >= std::mem::size_of_val(pids.as_slice()) {
            if pids.len() >= 65_536 {
                bail!("process group exceeds inspection bound");
            }
            pids.resize(pids.len() * 2, 0);
            continue;
        }
        for &pid in pids
            .iter()
            .take(bytes as usize / std::mem::size_of::<i32>())
            .filter(|&&pid| pid > 0)
        {
            let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
            unsafe {
                *libc::__error() = 0;
            }
            let size = std::mem::size_of_val(&info) as i32;
            let result = unsafe {
                libc::proc_pidinfo(
                    pid,
                    libc::PROC_PIDTBSDINFO,
                    0,
                    (&mut info as *mut libc::proc_bsdinfo).cast(),
                    size,
                )
            };
            if result == size {
                if info.pbi_pgid == pgid && info.pbi_status != libc::SZOMB {
                    return Ok(true);
                }
            } else {
                let error = std::io::Error::last_os_error();
                if !matches!(error.raw_os_error(), Some(0 | libc::ESRCH)) {
                    return Err(error.into());
                }
            }
        }
        return Ok(false);
    }
}

#[cfg(not(target_os = "macos"))]
pub fn group_has_live_members(pgid: u32) -> Result<bool> {
    let result = unsafe { libc::kill(-(pgid as i32), 0) };
    if result == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(false)
    } else {
        Err(error.into())
    }
}
