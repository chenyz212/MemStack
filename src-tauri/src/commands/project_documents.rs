//! 项目全局文档 Commands（执行计划 §10：草稿审核 / 晋升 / 同步状态 / 修复 / Embedding 设置）。
//!
//! 每个命令单一职责；不得使用 mode/action/force 标志参数复用命令。
//! 桌面固定 caller：与候选命令一致（桌面客户端，ReadWrite，无项目绑定）。

use tauri::State;

use crate::error::CommandError;
use crate::state::AppState;
use memory_domain::{
    ConclusionCardItem, ConclusionCardPayload, ProjectDocumentDraftItem, ProjectDocumentItem, ProjectDocumentOverview,
    ProjectDocumentType,
};

/// 项目文档状态总览（初始化状态 / 五份文档 / 草稿 / Embedding / 残留提示）。
#[tauri::command]
pub async fn get_project_document_overview(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<ProjectDocumentOverview, CommandError> {
    get_project_document_overview_impl(&state, &project_id)
}

/// 读取项目全部初始化草稿。
#[tauri::command]
pub async fn list_project_document_drafts(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<Vec<ProjectDocumentDraftItem>, CommandError> {
    list_project_document_drafts_impl(&state, &project_id)
}

/// 读取单份初始化草稿。
#[tauri::command]
pub async fn get_project_document_draft(
    state: State<'_, AppState>,
    project_id: String,
    document_type: ProjectDocumentType,
) -> Result<ProjectDocumentDraftItem, CommandError> {
    get_project_document_draft_impl(&state, &project_id, document_type)
}

/// 保存单份草稿编辑（版本递增 + 审核状态回退为待审核）。
#[tauri::command]
pub async fn save_project_document_draft(
    state: State<'_, AppState>,
    project_id: String,
    document_type: ProjectDocumentType,
    expected_version: i64,
    content: String,
) -> Result<ProjectDocumentDraftItem, CommandError> {
    save_project_document_draft_impl(&state, &project_id, document_type, expected_version, &content)
}

/// 批准单份草稿（记录批准版本）。
#[tauri::command]
pub async fn approve_project_document_draft(
    state: State<'_, AppState>,
    project_id: String,
    document_type: ProjectDocumentType,
) -> Result<ProjectDocumentDraftItem, CommandError> {
    approve_project_document_draft_impl(&state, &project_id, document_type)
}

/// 撤销单份草稿批准。
#[tauri::command]
pub async fn revoke_project_document_draft_approval(
    state: State<'_, AppState>,
    project_id: String,
    document_type: ProjectDocumentType,
) -> Result<ProjectDocumentDraftItem, CommandError> {
    revoke_project_document_draft_approval_impl(&state, &project_id, document_type)
}

/// 删除全部初始化草稿（行 + 磁盘目录）。
#[tauri::command]
pub async fn delete_project_document_drafts(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<(), CommandError> {
    delete_project_document_drafts_impl(&state, &project_id)
}

/// 执行五份草稿晋升（五份全部批准后；含中断恢复）。
#[tauri::command]
pub async fn promote_project_document_drafts(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<Vec<ProjectDocumentItem>, CommandError> {
    promote_project_document_drafts_impl(&state, &project_id)
}

/// 读取单份正式文档（只读镜像）。
#[tauri::command]
pub async fn get_project_document(
    state: State<'_, AppState>,
    project_id: String,
    document_type: ProjectDocumentType,
) -> Result<ProjectDocumentItem, CommandError> {
    get_project_document_impl(&state, &project_id, document_type)
}

/// 自动修复单份格式错误的文档（保留正文恢复 YAML；正文不可提取时从镜像恢复）。
#[tauri::command]
pub async fn repair_project_document(
    state: State<'_, AppState>,
    project_id: String,
    document_type: ProjectDocumentType,
) -> Result<ProjectDocumentItem, CommandError> {
    repair_project_document_impl(&state, &project_id, document_type)
}

/// 读取项目文档 Embedding 开关。
#[tauri::command]
pub async fn get_project_document_embedding_enabled(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<bool, CommandError> {
    get_project_document_embedding_enabled_impl(&state, &project_id)
}

/// 更新项目文档 Embedding 开关（显式传值；开启前界面负责显示隐私提示）。
#[tauri::command]
pub async fn set_project_document_embedding_enabled(
    state: State<'_, AppState>,
    project_id: String,
    enabled: bool,
) -> Result<bool, CommandError> {
    set_project_document_embedding_enabled_impl(&state, &project_id, enabled)
}

/// 读取结论卡片候选的结构化数据（供候选审核页字段编辑）。
#[tauri::command]
pub async fn get_conclusion_card_candidate_payload(
    state: State<'_, AppState>,
    candidate_id: String,
) -> Result<Option<ConclusionCardPayload>, CommandError> {
    get_conclusion_card_candidate_payload_impl(&state, &candidate_id)
}

/// 编辑结论卡片候选的结构化数据（重渲染正文并保持候选乐观锁）。
#[tauri::command]
pub async fn update_conclusion_card_candidate_payload(
    state: State<'_, AppState>,
    candidate_id: String,
    payload: ConclusionCardPayload,
    expected_version: i64,
) -> Result<memory_domain::MemoryCandidateItem, CommandError> {
    update_conclusion_card_candidate_payload_impl(&state, &candidate_id, &payload, expected_version)
}

/// 列出项目全部正式结论卡片。
#[tauri::command]
pub async fn list_conclusion_cards(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<Vec<ConclusionCardItem>, CommandError> {
    list_conclusion_cards_impl(&state, &project_id)
}

pub fn get_project_document_overview_impl(
    state: &AppState,
    project_id: &str,
) -> Result<ProjectDocumentOverview, CommandError> {
    Ok(state.documents.overview(project_id)?)
}

pub fn list_project_document_drafts_impl(
    state: &AppState,
    project_id: &str,
) -> Result<Vec<ProjectDocumentDraftItem>, CommandError> {
    Ok(state.documents.list_drafts_by_project(project_id)?)
}

pub fn get_project_document_draft_impl(
    state: &AppState,
    project_id: &str,
    document_type: ProjectDocumentType,
) -> Result<ProjectDocumentDraftItem, CommandError> {
    Ok(state.documents.get_draft(project_id, document_type)?)
}

pub fn save_project_document_draft_impl(
    state: &AppState,
    project_id: &str,
    document_type: ProjectDocumentType,
    expected_version: i64,
    content: &str,
) -> Result<ProjectDocumentDraftItem, CommandError> {
    Ok(state.documents.update_draft(
        project_id,
        document_type,
        expected_version,
        content,
        "用户在 MemStack 中编辑草稿",
    )?)
}

pub fn approve_project_document_draft_impl(
    state: &AppState,
    project_id: &str,
    document_type: ProjectDocumentType,
) -> Result<ProjectDocumentDraftItem, CommandError> {
    Ok(state.documents.approve_draft(project_id, document_type)?)
}

pub fn revoke_project_document_draft_approval_impl(
    state: &AppState,
    project_id: &str,
    document_type: ProjectDocumentType,
) -> Result<ProjectDocumentDraftItem, CommandError> {
    Ok(state.documents.revoke_draft_approval(project_id, document_type)?)
}

pub fn delete_project_document_drafts_impl(state: &AppState, project_id: &str) -> Result<(), CommandError> {
    Ok(state.documents.delete_drafts(project_id)?)
}

pub fn promote_project_document_drafts_impl(
    state: &AppState,
    project_id: &str,
) -> Result<Vec<ProjectDocumentItem>, CommandError> {
    Ok(state.documents.promote_drafts(project_id)?)
}

pub fn get_project_document_impl(
    state: &AppState,
    project_id: &str,
    document_type: ProjectDocumentType,
) -> Result<ProjectDocumentItem, CommandError> {
    Ok(state.documents.get_document(project_id, document_type)?)
}

pub fn repair_project_document_impl(
    state: &AppState,
    project_id: &str,
    document_type: ProjectDocumentType,
) -> Result<ProjectDocumentItem, CommandError> {
    Ok(state.documents.repair_document(project_id, document_type)?)
}

pub fn get_project_document_embedding_enabled_impl(state: &AppState, project_id: &str) -> Result<bool, CommandError> {
    let (_, enabled) = state.documents.get_project_settings(project_id)?;
    Ok(enabled)
}

pub fn set_project_document_embedding_enabled_impl(
    state: &AppState,
    project_id: &str,
    enabled: bool,
) -> Result<bool, CommandError> {
    Ok(state.documents.set_embedding_enabled(project_id, enabled)?)
}

pub fn get_conclusion_card_candidate_payload_impl(
    state: &AppState,
    candidate_id: &str,
) -> Result<Option<ConclusionCardPayload>, CommandError> {
    Ok(state.conclusion_cards.get_candidate_payload(candidate_id)?)
}

pub fn update_conclusion_card_candidate_payload_impl(
    state: &AppState,
    candidate_id: &str,
    payload: &ConclusionCardPayload,
    expected_version: i64,
) -> Result<memory_domain::MemoryCandidateItem, CommandError> {
    Ok(state
        .conclusion_cards
        .update_candidate_payload(candidate_id, payload, expected_version)?)
}

pub fn list_conclusion_cards_impl(state: &AppState, project_id: &str) -> Result<Vec<ConclusionCardItem>, CommandError> {
    Ok(state.conclusion_cards.list_cards_for_project(project_id)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use memory_domain::ALL_PROJECT_DOCUMENT_TYPES;

    fn state_with_bound_workspace() -> (AppState, tempfile::TempDir, tempfile::TempDir, String) {
        let temp = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let database_path = temp.path().join("test.db");
        drop(memory_storage::open_initialized(&database_path).unwrap());
        let state = AppState::build(database_path).unwrap();
        // 建项目并绑定工作空间。
        let project = state
            .projects
            .create(&memory_domain::SaveProjectRequest {
                name: "命令测试项目".to_string(),
                description: String::new(),
                color: "#238f7a".to_string(),
            })
            .unwrap();
        state
            .workspaces
            .bind_workspace(&project.id, workspace.path().to_str().unwrap())
            .unwrap();
        (state, temp, workspace, project.id)
    }

    fn five_documents() -> Vec<(ProjectDocumentType, String)> {
        ALL_PROJECT_DOCUMENT_TYPES
            .into_iter()
            .map(|kind| (kind, format!("# {}\n\n初始内容。", kind.display_name())))
            .collect()
    }

    #[test]
    fn draft_review_and_promotion_commands() {
        let (state, _temp, workspace, project_id) = state_with_bound_workspace();
        let workspace_path = workspace.path().to_str().unwrap().to_string();
        state
            .documents
            .create_drafts(&workspace_path, &five_documents(), Some("初始创建"))
            .unwrap();

        // 总览：草稿待审核。
        let overview = get_project_document_overview_impl(&state, &project_id).unwrap();
        assert_eq!(overview.status, "DRAFT_PENDING_REVIEW");
        assert_eq!(overview.drafts.len(), 5);

        // 编辑已批准草稿 → 自动撤销批准。
        approve_project_document_draft_impl(&state, &project_id, ProjectDocumentType::Context).unwrap();
        let draft = get_project_document_draft_impl(&state, &project_id, ProjectDocumentType::Context).unwrap();
        save_project_document_draft_impl(
            &state,
            &project_id,
            ProjectDocumentType::Context,
            draft.version,
            "# 项目背景\n\n用户改。",
        )
        .unwrap();
        let updated = get_project_document_draft_impl(&state, &project_id, ProjectDocumentType::Context).unwrap();
        assert_eq!(updated.review_status, "PENDING_REVIEW");
        // 撤销批准命令。
        approve_project_document_draft_impl(&state, &project_id, ProjectDocumentType::Context).unwrap();
        revoke_project_document_draft_approval_impl(&state, &project_id, ProjectDocumentType::Context).unwrap();
        let revoked = get_project_document_draft_impl(&state, &project_id, ProjectDocumentType::Context).unwrap();
        assert_eq!(revoked.review_status, "PENDING_REVIEW");

        // 五份批准 → 晋升 → ACTIVE。
        for kind in ALL_PROJECT_DOCUMENT_TYPES {
            approve_project_document_draft_impl(&state, &project_id, kind).unwrap();
        }
        let documents = promote_project_document_drafts_impl(&state, &project_id).unwrap();
        assert_eq!(documents.len(), 5);
        let overview = get_project_document_overview_impl(&state, &project_id).unwrap();
        assert_eq!(overview.status, "ACTIVE");
        assert_eq!(overview.documents.len(), 5);

        // 正式文档只读读取 + Embedding 设置。
        let document = get_project_document_impl(&state, &project_id, ProjectDocumentType::Decisions).unwrap();
        assert!(document.content.contains("初始内容"));
        assert!(!get_project_document_embedding_enabled_impl(&state, &project_id).unwrap());
        set_project_document_embedding_enabled_impl(&state, &project_id, true).unwrap();
        assert!(get_project_document_embedding_enabled_impl(&state, &project_id).unwrap());
    }

    #[test]
    fn delete_drafts_command_clears_everything() {
        let (state, _temp, workspace, project_id) = state_with_bound_workspace();
        state
            .documents
            .create_drafts(workspace.path().to_str().unwrap(), &five_documents(), None)
            .unwrap();
        delete_project_document_drafts_impl(&state, &project_id).unwrap();
        assert!(
            list_project_document_drafts_impl(&state, &project_id)
                .unwrap()
                .is_empty()
        );
        let overview = get_project_document_overview_impl(&state, &project_id).unwrap();
        assert_eq!(overview.status, "NOT_INITIALIZED");
    }

    #[test]
    fn conclusion_card_candidate_payload_commands() {
        let (state, _temp, workspace, _project_id) = state_with_bound_workspace();
        let payload = ConclusionCardPayload {
            title: "标题".to_string(),
            problem_id: None,
            problem_description: "问题描述".to_string(),
            final_conclusion: "结论".to_string(),
            root_cause: "根因".to_string(),
            applicable_conditions: vec![],
            not_applicable_conditions: vec![],
            evidence: vec!["证据".to_string()],
            verified_results: vec!["验证".to_string()],
            failed_attempts: vec![],
            do_not_repeat: vec![],
            retry_conditions: vec![],
            next_steps: vec![],
            keywords: vec![],
            tags: vec![],
            importance: 3,
            importance_reason: "一般".to_string(),
            cloud_embedding_allowed: false,
            resolved_at: "2026-08-21T08:00:00Z".to_string(),
        };
        let candidate = state
            .conclusion_cards
            .submit_candidate(workspace.path().to_str().unwrap(), &payload, "Codex")
            .unwrap();
        // 读取载荷。
        let stored = get_conclusion_card_candidate_payload_impl(&state, &candidate.id)
            .unwrap()
            .expect("结论卡片候选必须有载荷");
        assert_eq!(stored.title, "标题");
        // 编辑载荷（版本递增 + 正文重渲染）。
        let mut edited = payload.clone();
        edited.final_conclusion = "修订结论".to_string();
        let updated =
            update_conclusion_card_candidate_payload_impl(&state, &candidate.id, &edited, candidate.version).unwrap();
        assert_eq!(updated.version, 2);
        assert!(updated.content.contains("修订结论"));
        // 普通候选无载荷。
        let plain = state
            .candidates
            .submit(
                &memory_domain::SaveMemoryCandidateRequest {
                    scope: memory_domain::MemoryScope::Personal,
                    project_id: None,
                    title: "普通候选".to_string(),
                    summary: String::new(),
                    content: "内容".to_string(),
                    memory_type: "NOTE".to_string(),
                    keywords: vec![],
                    tags: vec![],
                    importance: 3,
                    cloud_processing_allowed: false,
                    expected_version: None,
                },
                "桌面",
            )
            .unwrap();
        assert!(
            get_conclusion_card_candidate_payload_impl(&state, &plain.id)
                .unwrap()
                .is_none()
        );
    }
}
