use anyhow::{Context, Result};
#[cfg(feature = "usb")]
use idevice::ReadWrite;
#[cfg(feature = "usb")]
use idevice::usbmuxd::UsbmuxdConnection;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;

pub enum Stream {
    Tcp(TcpStream),
    #[cfg(feature = "usb")]
    Usb(Box<dyn ReadWrite>),
}

impl AsyncRead for Stream {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            Stream::Tcp(s) => std::pin::Pin::new(s).poll_read(cx, buf),
            #[cfg(feature = "usb")]
            Stream::Usb(s) => std::pin::Pin::new(s.as_mut()).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for Stream {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        match self.get_mut() {
            Stream::Tcp(s) => std::pin::Pin::new(s).poll_write(cx, buf),
            #[cfg(feature = "usb")]
            Stream::Usb(s) => std::pin::Pin::new(s.as_mut()).poll_write(cx, buf),
        }
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            Stream::Tcp(s) => std::pin::Pin::new(s).poll_flush(cx),
            #[cfg(feature = "usb")]
            Stream::Usb(s) => std::pin::Pin::new(s.as_mut()).poll_flush(cx),
        }
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            Stream::Tcp(s) => std::pin::Pin::new(s).poll_shutdown(cx),
            #[cfg(feature = "usb")]
            Stream::Usb(s) => std::pin::Pin::new(s.as_mut()).poll_shutdown(cx),
        }
    }
}

#[cfg(feature = "usb")]
pub async fn connect_usb(port: u16) -> Result<Stream> {
    let mut muxd = UsbmuxdConnection::default()
        .await
        .map_err(|e| anyhow::anyhow!("{e:?}"))
        .context("Failed to connect to usbmuxd")?;

    let devices = muxd
        .get_devices()
        .await
        .map_err(|e| anyhow::anyhow!("{e:?}"))
        .context("Failed to list devices")?;

    let device = devices.first().context("No iOS device found")?;
    if devices.len() > 1 {
        eprintln!(
            "Warning: {} devices connected, using {}",
            devices.len(),
            device.udid
        );
    }
    eprintln!("Found device: {}", device.udid);

    let idevice = muxd
        .connect_to_device(device.device_id, port, "iosmic")
        .await
        .map_err(|e| anyhow::anyhow!("{e:?}"))
        .context("Failed to connect to device")?;

    let socket = idevice.get_socket().context("No socket in device")?;
    Ok(Stream::Usb(socket))
}

pub async fn connect_wifi(host: &str, port: u16) -> Result<Stream> {
    let stream = TcpStream::connect((host, port))
        .await
        .context("Failed to connect via WiFi")?;
    eprintln!("Connected to {}:{}", host, port);
    Ok(Stream::Tcp(stream))
}
