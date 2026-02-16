use std::sync::Arc;

use agent_client_protocol as acp;
use anyhow::Result;
use chrono::Utc;
use gpui::{App, SharedString, Task, WeakEntity};
use language_model::LanguageModelToolResultContent;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{AgentTool, Thread, Todo, TodoPriority, TodoStatus, ToolCallEventStream};

/// Manages todo items for the current thread.
///
/// Use this tool to keep track of tasks that need to be completed.
/// IMPORTANT:
/// - When you add, update, or complete a task, read and display the updated todo list.
/// - When you START working on a task, update its status to "in_progress" so you can track what you're currently working on.
///
/// Examples:
/// - Read all todos: {"command": "read_todos"}
/// - Add a todo: {"command": "add_todo", "text": "Fix the login bug", "priority": "high"}
/// - Start working on a task: {"command": "update_todo", "id": "abc-123", "status": "in_progress"}
/// - Complete a task: {"command": "update_todo", "id": "abc-123", "status": "completed"}
/// - Delete a todo: {"command": "delete_todo", "id": "abc-123"}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct TodoToolInput {
    /// The command to execute: "read_todos", "add_todo", "update_todo", or "delete_todo"
    pub command: String,
    /// Text for the todo (required for add_todo)
    #[serde(default)]
    pub text: Option<String>,
    /// Todo ID (required for update_todo and delete_todo)
    #[serde(default)]
    pub id: Option<String>,
    /// Status: "pending", "in_progress", or "completed" (for update_todo)
    #[serde(default)]
    pub status: Option<String>,
    /// Priority: "low", "medium", or "high" (for add_todo and update_todo)
    #[serde(default)]
    pub priority: Option<String>,
}

pub struct TodoTool {
    thread: WeakEntity<Thread>,
}

impl TodoTool {
    pub fn new(thread: WeakEntity<Thread>) -> Self {
        Self { thread }
    }
}

impl AgentTool for TodoTool {
    type Input = TodoToolInput;
    type Output = LanguageModelToolResultContent;

    const NAME: &'static str = "todo";

    fn kind() -> acp::ToolKind {
        acp::ToolKind::Other
    }

    fn initial_title(
        &self,
        input: Result<Self::Input, serde_json::Value>,
        _cx: &mut App,
    ) -> SharedString {
        match input {
            Ok(i) => match i.command.as_str() {
                "read_todos" => "Read todos".into(),
                "add_todo" => "Add todo".into(),
                "update_todo" => "Update todo".into(),
                "delete_todo" => "Delete todo".into(),
                _ => "Manage todos".into(),
            },
            Err(_) => "Manage todos".into(),
        }
    }

    fn run(
        self: Arc<Self>,
        input: Self::Input,
        _event_stream: ToolCallEventStream,
        cx: &mut App,
    ) -> Task<Result<Self::Output>> {
        let thread = self.thread.clone();
        cx.spawn(async move |cx| match input.command.as_str() {
            "read_todos" => {
                let todos =
                    thread.read_with(cx, |thread: &Thread, _| format_todos(&thread.todos))?;
                Ok(todos.into())
            }
            "add_todo" => {
                let text = input
                    .text
                    .ok_or_else(|| anyhow::anyhow!("text is required for add_todo"))?;
                let priority = input
                    .priority
                    .as_ref()
                    .and_then(|p| parse_priority(p))
                    .unwrap_or(TodoPriority::Medium);
                let id = Uuid::new_v4().to_string();
                let todo = Todo {
                    id: id.clone(),
                    text,
                    status: TodoStatus::Pending,
                    priority,
                    created_at: Utc::now(),
                };
                thread.update(cx, |thread: &mut Thread, _| {
                    thread.todos.push(todo);
                })?;
                let todos =
                    thread.read_with(cx, |thread: &Thread, _| format_todos(&thread.todos))?;
                Ok(todos.into())
            }
            "update_todo" => {
                let id = input
                    .id
                    .ok_or_else(|| anyhow::anyhow!("id is required for update_todo"))?;
                let mut found = false;
                thread.update(cx, |thread: &mut Thread, _| {
                    if let Some(todo) = thread.todos.iter_mut().find(|t| t.id == id) {
                        if let Some(status) = &input.status {
                            if let Some(s) = parse_status(status) {
                                todo.status = s;
                            }
                        }
                        if let Some(priority) = &input.priority {
                            if let Some(p) = parse_priority(priority) {
                                todo.priority = p;
                            }
                        }
                        if let Some(text) = &input.text {
                            todo.text = text.clone();
                        }
                        found = true;
                    }
                })?;
                let todos =
                    thread.read_with(cx, |thread: &Thread, _| format_todos(&thread.todos))?;
                if found {
                    Ok(todos.into())
                } else {
                    Ok(format!("Todo with id {} not found", id).into())
                }
            }
            "delete_todo" => {
                let delete_id = input
                    .id
                    .ok_or_else(|| anyhow::anyhow!("id is required for delete_todo"))?;
                let mut found = false;
                thread.update(cx, |thread: &mut Thread, _| {
                    let initial_len = thread.todos.len();
                    thread.todos.retain(|t| t.id != delete_id);
                    found = thread.todos.len() < initial_len;
                })?;
                let todos =
                    thread.read_with(cx, |thread: &Thread, _| format_todos(&thread.todos))?;
                if found {
                    Ok(todos.into())
                } else {
                    Ok(format!("Todo with id {} not found", delete_id).into())
                }
            }
            _ => Ok(format!(
                "Unknown command: {}. Use: read_todos, add_todo, update_todo, or delete_todo",
                input.command
            )
            .into()),
        })
    }
}

fn parse_status(s: &str) -> Option<TodoStatus> {
    match s.to_lowercase().as_str() {
        "pending" => Some(TodoStatus::Pending),
        "in_progress" => Some(TodoStatus::InProgress),
        "completed" => Some(TodoStatus::Completed),
        _ => None,
    }
}

fn parse_priority(p: &str) -> Option<TodoPriority> {
    match p.to_lowercase().as_str() {
        "low" => Some(TodoPriority::Low),
        "medium" => Some(TodoPriority::Medium),
        "high" => Some(TodoPriority::High),
        _ => None,
    }
}

fn format_todos(todos: &[Todo]) -> String {
    if todos.is_empty() {
        return "No todos yet. Use the todo tool with command: 'add_todo' to create one."
            .to_string();
    }

    let mut output = String::from("# Todos\n\n");

    let mut completed: Vec<&Todo> = todos
        .iter()
        .filter(|t| matches!(t.status, TodoStatus::Completed))
        .collect();
    let mut in_progress: Vec<&Todo> = todos
        .iter()
        .filter(|t| matches!(t.status, TodoStatus::InProgress))
        .collect();
    let mut pending: Vec<&Todo> = todos
        .iter()
        .filter(|t| matches!(t.status, TodoStatus::Pending))
        .collect();

    completed.sort_by(|a, b| b.priority.cmp(&a.priority));
    in_progress.sort_by(|a, b| b.priority.cmp(&a.priority));
    pending.sort_by(|a, b| b.priority.cmp(&a.priority));

    if !completed.is_empty() {
        output.push_str("## Completed\n\n");
        for todo in &completed {
            output.push_str(&format!(
                "- [x] [{}] {} - `{}`\n",
                priority_to_string(&todo.priority),
                todo.text,
                todo.id
            ));
        }
        output.push('\n');
    }

    if !in_progress.is_empty() {
        output.push_str("## In Progress\n\n");
        for todo in &in_progress {
            output.push_str(&format!(
                "- 🏃 [{}] {} - `{}`\n",
                priority_to_string(&todo.priority),
                todo.text,
                todo.id
            ));
        }
        output.push('\n');
    }

    if !pending.is_empty() {
        output.push_str("## Pending\n\n");
        for todo in &pending {
            output.push_str(&format!(
                "- [ ] [{}] {} - `{}`\n",
                priority_to_string(&todo.priority),
                todo.text,
                todo.id
            ));
        }
    }

    output
}

fn priority_to_string(priority: &TodoPriority) -> &'static str {
    match priority {
        TodoPriority::Low => "low",
        TodoPriority::Medium => "medium",
        TodoPriority::High => "high",
    }
}
