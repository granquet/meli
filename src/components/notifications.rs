/*
 * meli
 *
 * Copyright 2017-2018 Manos Pitsidianakis
 *
 * This file is part of meli.
 *
 * meli is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 *
 * meli is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with meli. If not, see <http://www.gnu.org/licenses/>.
 */

/*!
Notification handling components.
*/
use std::process::{Command, Stdio};

#[cfg(all(target_os = "linux", feature = "dbus-notifications"))]
pub use dbus::*;

use super::*;

#[cfg(all(target_os = "linux", feature = "dbus-notifications"))]
mod dbus {
    use super::*;
    use crate::types::RateLimit;

    /// Passes notifications to the OS using Dbus
    #[derive(Debug)]
    pub struct DbusNotifications {
        rate_limit: RateLimit,
        id: ComponentId,
    }

    impl fmt::Display for DbusNotifications {
        fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
            write!(f, "")
        }
    }

    impl DbusNotifications {
        pub fn new(context: &Context) -> Self {
            DbusNotifications {
                rate_limit: RateLimit::new(1000, 1000, context.job_executor.clone()),
                id: ComponentId::default(),
            }
        }
    }

    impl Component for DbusNotifications {
        fn draw(&mut self, _grid: &mut CellBuffer, _area: Area, _context: &mut Context) {}

        fn process_event(&mut self, event: &mut UIEvent, context: &mut Context) -> bool {
            if !context.settings.notifications.enable {
                return false;
            }

            if let UIEvent::Notification(ref title, ref body, ref kind) = event {
                if !self.rate_limit.tick() {
                    return false;
                }

                let settings = &context.settings.notifications;
                let mut notification = notify_rust::Notification::new();
                notification
                    .appname("meli")
                    .summary(title.as_ref().map(String::as_str).unwrap_or("meli"))
                    .body(&escape_str(body));
                match *kind {
                    Some(NotificationType::NewMail) => {
                        notification.hint(notify_rust::Hint::Category("email".to_owned()));
                        notification.icon("mail-message-new");
                        notification.sound_name("message-new-email");
                    }
                    Some(NotificationType::SentMail) => {
                        notification.hint(notify_rust::Hint::Category("email".to_owned()));
                        notification.icon("mail-send");
                        notification.sound_name("message-sent-email");
                    }
                    Some(NotificationType::Saved) => {
                        notification.icon("document-save");
                    }
                    Some(NotificationType::Info) => {
                        notification.icon("dialog-information");
                    }
                    Some(NotificationType::Error(melib::ErrorKind::Authentication)) => {
                        notification.icon("dialog-password");
                    }
                    Some(NotificationType::Error(melib::ErrorKind::Bug)) => {
                        notification.icon("face-embarrassed");
                    }
                    Some(NotificationType::Error(melib::ErrorKind::None))
                    | Some(NotificationType::Error(melib::ErrorKind::External)) => {
                        notification.icon("dialog-error");
                    }
                    Some(NotificationType::Error(melib::ErrorKind::Network(_))) => {
                        notification.icon("network-error");
                    }
                    Some(NotificationType::Error(melib::ErrorKind::Timeout)) => {
                        notification.icon("network-offline");
                    }
                    _ => {}
                }
                if settings.play_sound.is_true() {
                    if let Some(ref sound_path) = settings.sound_file {
                        notification.hint(notify_rust::Hint::SoundFile(sound_path.to_owned()));
                    }
                } else {
                    notification.hint(notify_rust::Hint::SuppressSound(true));
                }

                if let Err(err) = notification.show() {
                    log::debug!("Could not show dbus notification: {:?}", &err);
                    log::error!("Could not show dbus notification: {err}");
                }
            }
            false
        }

        fn set_dirty(&mut self, _value: bool) {}

        fn is_dirty(&self) -> bool {
            false
        }

        fn id(&self) -> ComponentId {
            self.id
        }

        fn set_id(&mut self, id: ComponentId) {
            self.id = id;
        }
    }

    fn escape_str(s: &str) -> String {
        let mut ret: String = String::with_capacity(s.len());
        for c in s.chars() {
            match c {
                '&' => ret.push_str("&amp;"),
                '<' => ret.push_str("&lt;"),
                '>' => ret.push_str("&gt;"),
                '\'' => ret.push_str("&apos;"),
                '"' => ret.push_str("&quot;"),
                _ => {
                    let i = c as u32;
                    if (0x1..=0x8).contains(&i)
                        || (0xb..=0xc).contains(&i)
                        || (0xe..=0x1f).contains(&i)
                        || (0x7f..=0x84).contains(&i)
                        || (0x86..=0x9f).contains(&i)
                    {
                        use std::fmt::Write;
                        let _ = write!(ret, "&#{:x}%{:x};", i, i);
                    } else {
                        ret.push(c);
                    }
                }
            }
        }
        ret
    }
}

/// Passes notifications to a user defined shell command
#[derive(Default, Debug)]
pub struct NotificationCommand {
    id: ComponentId,
}

impl NotificationCommand {
    pub fn new() -> Self {
        NotificationCommand::default()
    }
}

impl fmt::Display for NotificationCommand {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "")
    }
}

impl Component for NotificationCommand {
    fn draw(&mut self, _grid: &mut CellBuffer, _area: Area, _context: &mut Context) {}

    fn process_event(&mut self, event: &mut UIEvent, context: &mut Context) -> bool {
        if let UIEvent::Notification(ref title, ref body, ref kind) = event {
            if context.settings.notifications.enable {
                if *kind == Some(NotificationType::NewMail) {
                    if let Some(ref path) = context.settings.notifications.xbiff_file_path {
                        if let Err(err) = update_xbiff(path) {
                            log::debug!("Could not update xbiff file: {:?}", &err);
                            log::error!("Could not update xbiff file: {err}.");
                        }
                    }
                }

                let mut script = context.settings.notifications.script.as_ref();
                if *kind == Some(NotificationType::NewMail)
                    && context.settings.notifications.new_mail_script.is_some()
                {
                    script = context.settings.notifications.new_mail_script.as_ref();
                }

                if let Some(ref bin) = script {
                    match Command::new(bin)
                        .arg(&kind.map(|k| k.to_string()).unwrap_or_default())
                        .arg(title.as_ref().map(String::as_str).unwrap_or("meli"))
                        .arg(body)
                        .stdin(Stdio::piped())
                        .stdout(Stdio::piped())
                        .spawn()
                    {
                        Ok(child) => {
                            context.children.push(child);
                        }
                        Err(err) => {
                            log::error!("Could not run notification script: {err}.");
                            log::debug!("Could not run notification script: {:?}", err);
                        }
                    }
                } else {
                    #[cfg(target_os = "macos")]
                    {
                        let applescript = format!(
                            "display notification \"{message}\" with title \"{title}\" subtitle \
                             \"{subtitle}\"",
                            message = body.replace('"', "'"),
                            title = title
                                .as_ref()
                                .map(String::as_str)
                                .unwrap_or("meli")
                                .replace('"', "'"),
                            subtitle = kind
                                .map(|k| k.to_string())
                                .unwrap_or_default()
                                .replace('"', "'")
                        );
                        match Command::new("osascript")
                            .arg("-e")
                            .arg(applescript)
                            .stdin(Stdio::piped())
                            .stdout(Stdio::piped())
                            .spawn()
                        {
                            Ok(child) => {
                                context.children.push(child);
                                return false;
                            }
                            Err(err) => {
                                log::error!("Could not run notification script: {err}.");
                                log::debug!("Could not run notification script: {:?}", err);
                            }
                        }
                    }

                    context
                        .replies
                        .push_back(UIEvent::StatusEvent(StatusEvent::DisplayMessage(format!(
                            "{title}{}{body}",
                            if title.is_some() { " " } else { "" },
                            title = title.as_deref().unwrap_or_default(),
                            body = body,
                        ))));
                }
            }
        }

        false
    }

    fn is_dirty(&self) -> bool {
        false
    }

    fn set_dirty(&mut self, _value: bool) {}

    fn id(&self) -> ComponentId {
        self.id
    }

    fn set_id(&mut self, id: ComponentId) {
        self.id = id;
    }
}

fn update_xbiff(path: &str) -> Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .append(true) /* writes will append to a file instead of overwriting previous contents */
        .create(true) /* a new file will be created if the file does not yet already exist. */
        .open(path)?;
    if file.metadata()?.len() > 128 {
        file.set_len(0)?;
    } else {
        std::io::Write::write_all(&mut file, b"z")?;
    }
    Ok(())
}
