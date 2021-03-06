use nix::unistd::ForkResult;
use std::os::unix::io::AsRawFd;
use std::os::unix::prelude::RawFd;
use tokio::net::TcpListener;

use futures::ready;
use std::pin::Pin;
use std::task::Context;
use std::task::Poll;
use tokio::io::unix::AsyncFd;
use tokio::io::AsyncRead;
use tokio::io::AsyncWrite;
use tokio::net::TcpStream;

fn set_nonblocking(fd: RawFd) -> Result<(), nix::Error> {
    let bits = nix::fcntl::fcntl(fd, nix::fcntl::FcntlArg::F_GETFL)?;
    let mut flags = nix::fcntl::OFlag::from_bits_truncate(bits);
    flags.insert(nix::fcntl::OFlag::O_NONBLOCK);
    nix::fcntl::fcntl(fd, nix::fcntl::FcntlArg::F_SETFL(flags))?;
    Ok(())
}

pub struct AsyncNixFd {
    fd: AsyncFd<RawFd>,
}

impl AsyncNixFd {
    pub fn new(fd: RawFd) -> std::io::Result<Self> {
        set_nonblocking(fd)?;
        Ok(Self {
            fd: AsyncFd::new(fd)?,
        })
    }
}

impl AsyncRead for AsyncNixFd {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        loop {
            let mut guard = ready!(self.fd.poll_read_ready(cx))?;

            match guard.try_io(|inner| {
                let n = nix::unistd::read(
                    inner.as_raw_fd(),
                    buf.initialize_unfilled(),
                )?;
                buf.advance(n);
                Ok(())
            }) {
                Ok(result) => return Poll::Ready(result),
                Err(_would_block) => continue,
            }
        }
    }
}

impl AsyncWrite for AsyncNixFd {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        loop {
            let mut guard = ready!(self.fd.poll_write_ready(cx))?;

            match guard
                .try_io(|inner| Ok(nix::unistd::write(inner.as_raw_fd(), buf)?))
            {
                Ok(result) => return Poll::Ready(result),
                Err(_would_block) => continue,
            }
        }
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

impl Drop for AsyncNixFd {
    fn drop(&mut self) {
        let _ = nix::unistd::close(self.fd.as_raw_fd());
    }
}

fn forkpty() -> Result<nix::pty::ForkptyResult, nix::Error> {
    unsafe { nix::pty::forkpty(None, None) }
}

async fn process(mut socket: TcpStream) {
    match forkpty() {
        Ok(pty) => match pty.fork_result {
            ForkResult::Child => {
                let _ = exec::Command::new("/usr/bin/login").exec();
            }
            ForkResult::Parent { child } => {
                let mut ptmx = AsyncNixFd::new(pty.master).unwrap();
                let _ =
                    tokio::io::copy_bidirectional(&mut ptmx, &mut socket).await;
                let _ = nix::sys::wait::waitpid(child, None);
            }
        },
        Err(e) => println!("forkpty failed; err = {:?}", e),
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("0.0.0.0:8888").await?;

    loop {
        let (socket, _) = listener.accept().await?;

        tokio::spawn(async move {
            process(socket).await;
        });
    }
}
