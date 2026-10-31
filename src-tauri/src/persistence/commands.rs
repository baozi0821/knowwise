//! M01 命令层(薄封装,`desktop` feature)。
//! 业务逻辑在 TaskService;此处只做请求校验与 DTO 转换。

use serde::Deserialize;

use crate::shared::paging::{Page, PageRequest};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskListRequest {
    #[serde(default)]
    pub document_id: Option<String>,
    #[serde(default)]
    pub active_only: bool,
    #[serde(flatten)]
    pub page: PageRequest,
}

#[tauri::command]
pub async fn task_list(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: TaskListRequest,
) -> Result<Page<crate::shared::dto::TaskView>, crate::shared::AppError> {
    let page = request.page.normalized();
    state.tasks.list_tasks(
        request.document_id,
        request.active_only,
        page.limit,
        page.cursor,
    )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskDeleteRequest {
    pub task_id: String,
}

#[tauri::command]
pub async fn task_delete(
    state: tauri::State<'_, crate::app_shell::AppState>,
    request: TaskDeleteRequest,
) -> Result<crate::shared::dto::DeleteResult, crate::shared::AppError> {
    state.tasks.delete_task(&request.task_id)
}

/// ts-rs 导出契约测试(命令返回类型)。
#[cfg(test)]
mod tests {
    use super::*;
    use ts_rs::TS;

    #[test]
    fn export_task_list_types() {
        // Page<TaskView> 由 Page.ts 泛型承载;TaskView 已在 shared 导出。
        assert!(Page::<crate::shared::dto::TaskView>::ident().contains("Page"));
    }
}
